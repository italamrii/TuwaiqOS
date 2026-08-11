# Unified acceptance harness.
#
# Runs the project's existing suites, adds a liveness probe of its own, and
# reduces everything to one verdict with the evidence attached.
#
# It orchestrates rather than replaces: each suite stays the authority on its
# own phase, and this script's job is to run them consistently, notice the
# failure modes a suite cannot report about itself (a panic, a deadlock, a
# kernel that stopped scheduling), and produce one machine-readable result.
#
#   PASS     every selected suite passed and the liveness probe was clean
#   PARTIAL  at least one suite passed and at least one was skipped, none failed
#   FAIL     any suite failed, or the liveness probe found a panic or a hang
#   SKIP     nothing could run (no image, no QEMU, no suites selected)
#
# Exit code is 0 for PASS, 1 for FAIL, 2 for PARTIAL, 3 for SKIP, so CI can
# branch on it without parsing the report.

[CmdletBinding()]
param(
    [string]$Image,
    [string]$OutDir,
    # Substring filter over suite names; omit to run everything registered.
    [string[]]$Suite,
    # Seconds allowed for the kernel to reach a shell prompt.
    [int]$BootTimeout = 180,
    [switch]$AllowDirty,
    # Run only the liveness probe. Useful as a fast pre-flight before the
    # long suites, and as the check a bisect script wants.
    [switch]$LivenessOnly,
    # Verify the failure detectors against captured serial logs. Boots
    # nothing, needs no image, and takes about a second.
    [switch]$SelfTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot

# --------------------------------------------------------------- registry
#
# `required` marks a suite whose absence is a FAIL rather than a SKIP. Adding
# a suite here is the only edit a new phase needs.
$Registry = @(
    [pscustomobject]@{ Name = "qemu-smoke";    Script = "scripts\qemu-smoke-test.ps1";      Required = $true  }
    [pscustomobject]@{ Name = "phase5";        Script = "scripts\phase5-acceptance.ps1";    Required = $false }
    [pscustomobject]@{ Name = "phase6";        Script = "scripts\phase6-smoke.ps1";         Required = $false }
    [pscustomobject]@{ Name = "phase6-storage"; Script = "scripts\phase6-storage-smoke.ps1"; Required = $false }
)

# ------------------------------------------------------------ preconditions

$Results = [System.Collections.Generic.List[object]]::new()
$Notes = [System.Collections.Generic.List[string]]::new()

function Add-Result {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][ValidateSet("PASS", "FAIL", "SKIP")][string]$Status,
        [string]$Evidence = "",
        [int]$DurationSeconds = 0,
        [string]$Artifacts = ""
    )
    $Results.Add([pscustomobject]@{
            name             = $Name
            status           = $Status
            evidence         = $Evidence
            duration_seconds = $DurationSeconds
            artifacts        = $Artifacts
        })
    $colour = switch ($Status) { "PASS" { "Green" } "FAIL" { "Red" } default { "Yellow" } }
    Write-Host ("[{0,-4}] {1}{2}" -f $Status, $Name, $(if ($Evidence) { " - $Evidence" } else { "" })) -ForegroundColor $colour
}

if (-not $Image) { $Image = Join-Path $ProjectRoot "target\debug\boot-bios-tuwaiqos.img" }
$Image = [System.IO.Path]::GetFullPath($Image)

# -SelfTest exercises the detectors only. It deliberately skips every
# precondition below -- no image, no QEMU, no clean worktree required -- so it
# stays runnable on a machine that cannot boot anything, and in CI as a
# pre-flight before the expensive stages.
if (-not $SelfTest) {
    $Dirty = @(& git status --porcelain=v1 --untracked-files=all)
    if ($Dirty.Count -gt 0 -and -not $AllowDirty) {
        throw "Worktree is dirty; commit the candidate or pass -AllowDirty for development."
    }

    $Commit = (& git rev-parse HEAD).Trim()
    $Timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
    if (-not $OutDir) {
        $OutDir = Join-Path $ProjectRoot "target\acceptance\$($Commit.Substring(0, 12))-$Timestamp"
    }
    $OutDir = [System.IO.Path]::GetFullPath($OutDir)
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

    $ResultsJson = Join-Path $OutDir "results.json"
    $ManifestJson = Join-Path $OutDir "manifest.json"
    $ReportPath = Join-Path $OutDir "report.txt"

    Write-Host "=== TuwaiqOS acceptance ===" -ForegroundColor Cyan
    Write-Host "commit : $Commit"
    Write-Host "image  : $Image"
    Write-Host "output : $OutDir"
    Write-Host ""

    $QemuCommand = Get-Command qemu-system-x86_64 -ErrorAction SilentlyContinue
    $Qemu = if ($QemuCommand) { $QemuCommand.Source } else { "C:\Program Files\qemu\qemu-system-x86_64.exe" }
    $QemuPresent = Test-Path -LiteralPath $Qemu -PathType Leaf
    $ImagePresent = Test-Path -LiteralPath $Image -PathType Leaf

    if (-not $ImagePresent) { $Notes.Add("disk image not found at '$Image' -- run scripts\build.ps1") }
    if (-not $QemuPresent) { $Notes.Add("qemu-system-x86_64 not found in PATH") }
}

# --------------------------------------------------------- liveness probe
#
# What a suite cannot report about itself. Boots the image and answers three
# questions from the serial log:
#
#   1. did the kernel panic?
#   2. did it reach a shell prompt inside the timeout?
#   3. is the scheduler still running -- i.e. is the heartbeat task ticking at
#      the rate the PIT implies?
#
# (3) is the one that catches a live-but-wedged kernel: the `heartbeat` task
# sleeps `sleep_ticks(100)` at a 100 Hz PIT, so it emits one line per second of
# *interrupt* time. Counting those lines over a known wall-clock window
# measures how many timer interrupts actually reached the CPU. A kernel that
# has disabled interrupts, deadlocked in an ISR, or stopped scheduling shows a
# heartbeat rate far below 1 Hz while still appearing "up".

# The two judgements are kept as pure functions, separate from the code that
# drives QEMU, so `-SelfTest` can exercise them against captured serial logs
# without booting anything. A detector that has never been shown a failure is
# not a detector; see `tests/fixtures/serial/`.

# Regexes live here rather than inline so the self-test and the probe can
# never diverge.

# Unconditionally fatal. The panic handler writes the `=== KERNEL PANIC ===`
# block to serial, and a double fault has no recovery path at all.
$Script:PanicPattern = 'KERNEL PANIC|EXCEPTION: DOUBLE FAULT|memory allocation of \d+ bytes failed'

# Faults that are fatal or benign depending on where they came from.
#
# Since Phase 4, each of these handlers checks the saved CS: an RPL of 3 means
# a user program did something its own mappings forbid, so the kernel ends that
# process and carries on -- which is correct behaviour, and exactly what a
# Ring-3 isolation test is *supposed* to produce. A Ring-0-origin fault takes
# the old path: `report_fault` then an unconditional halt.
#
# Both print the same `EXCEPTION: ...` line first, so the line alone cannot
# tell them apart. Only the recovery notice that follows can. Treating the
# exception as fatal on sight would report a working kernel as dead every time
# a test faults a user process on purpose.
#
# Comparing counts rather than searching for adjacency keeps this correct when
# several faults occur in one run: if any exception of a given kind lacks its
# recovery notice, one of them was not recovered.
#
# Source: kernel/src/interrupts.rs -- page_fault_handler,
# general_protection_fault_handler, invalid_opcode_handler,
# divide_error_handler. `report_fault` writes to the framebuffer only, so on a
# serial-only capture an unrecovered fault leaves no further trace: silence
# after the exception is the signal.
$Script:ConditionalFaults = @(
    @{ Name = 'page fault'
        Exception = 'EXCEPTION: PAGE FAULT'
        Recovery  = 'usermode: page fault trapped safely from CPL=3'
    }
    @{ Name = 'general protection fault'
        Exception = 'EXCEPTION: GENERAL PROTECTION FAULT'
        Recovery  = 'usermode: privileged instruction trapped safely from CPL=3'
    }
    @{ Name = 'invalid opcode'
        Exception = 'EXCEPTION: INVALID OPCODE'
        Recovery  = 'usermode: invalid opcode trapped safely from CPL=3'
    }
    @{ Name = 'divide error'
        Exception = 'EXCEPTION: DIVIDE ERROR'
        Recovery  = 'usermode: divide error trapped safely from CPL=3'
    }
)
$Script:ShellPattern = 'shell: command begin|tuwaiq@os|TuwaiqOS shell'
$Script:BeatPattern = 'beat #'
# "beat number 3 or higher": single digits 3-9, or any number of two digits or
# more. Writing it as `[3-9]` alone would silently fail to match `beat #97`,
# because the word boundary after `9` never lands mid-number.
$Script:ThirdBeatPattern = 'beat #(?:[3-9]|\d{2,})\b'

# A suite that could not start is not evidence about the kernel. These are the
# signatures of "never got as far as testing anything", and they map to SKIP
# with the reason preserved rather than to FAIL.
#
# The list is explicit rather than a catch-all on purpose: anything not matched
# here stays a FAIL. Misreporting a real failure as a skip is far worse than
# the reverse, so the default has to be pessimistic.
$Script:CannotStartPatterns = @(
    @{ Pattern = 'is not supported for the cmdlet .* on this edition of PowerShell'
        Reason  = 'suite uses Windows-only PowerShell features; not runnable on this host'
    }
    @{ Pattern = 'must run on the .*? branch|Acceptance must run on'
        Reason  = 'suite refused to run on this branch'
    }
    # Anchored to the suites' own wording. An earlier draft used
    # `image not found` and `qemu.*not found`, which are loose enough to match
    # a genuine assertion failure such as "expected image not found on disk" --
    # and would then have downgraded a real failure to a skip.
    @{ Pattern = 'Disk image not found'
        Reason  = 'suite could not find a disk image'
    }
    @{ Pattern = 'qemu-system-x86_64[^\r\n]*not found|QEMU not found|not found in PATH'
        Reason  = 'QEMU not available to the suite'
    }
)

function Get-SuiteStartFailure {
    <#
    .SYNOPSIS
    If the suite's output shows it never started, return why. Otherwise $null.
    #>
    param([AllowEmptyString()][AllowNull()][string]$Text)

    if (-not $Text) { return $null }
    foreach ($signature in $Script:CannotStartPatterns) {
        if ($Text -match $signature.Pattern) { return $signature.Reason }
    }
    $null
}

function Get-SerialState {
    <#
    .SYNOPSIS
    Classify a serial capture. Pure: text in, verdict fields out.
    #>
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Text)

    # A CPL=3 fault that the kernel trapped and contained is a success story,
    # not a crash: count the exceptions and their recovery notices per kind,
    # and treat only the unmatched ones as fatal.
    $unrecovered = [System.Collections.Generic.List[string]]::new()
    $contained = 0
    foreach ($fault in $Script:ConditionalFaults) {
        $raised = ([regex]::Matches($Text, [regex]::Escape($fault.Exception))).Count
        $recovered = ([regex]::Matches($Text, [regex]::Escape($fault.Recovery))).Count
        $contained += $recovered
        if ($raised -gt $recovered) {
            $unrecovered.Add("$($raised - $recovered) unrecovered $($fault.Name) (Ring-0)")
        }
    }

    $panicked = ($Text -match $Script:PanicPattern) -or ($unrecovered.Count -gt 0)
    $panicLine = ""
    if ($unrecovered.Count -gt 0) {
        $panicLine = $unrecovered -join '; '
    }
    if ($Text -match $Script:PanicPattern) {
        # The kernel prints a delimited block:
        #
        #   === KERNEL PANIC ===
        #   panicked at kernel/src/foo.rs:115:9:
        #   <message>
        #   =====================
        #
        # The banner alone says nothing, so lift the body -- location and
        # message are what a reader needs. Report the first block, not the
        # last: later ones are usually consequences of the first.
        $block = [regex]::Match($Text, '(?s)=== KERNEL PANIC ===\r?\n(.*?)\r?\n=+\r?\n')
        if ($block.Success) {
            $panicLine = (($block.Groups[1].Value -split '\r?\n' |
                    Where-Object { $_.Trim() }) -join ' | ').Trim()
        }
        else {
            # An exception handler or a truncated capture: fall back to the
            # first line that names a fault.
            $panicLine = ([regex]::Match(
                    $Text,
                    '(?m)^.*(KERNEL PANIC|DOUBLE FAULT|PAGE FAULT|allocation of \d+ bytes failed).*$'
                )).Value.Trim()
        }
    }

    [pscustomobject]@{
        Panicked     = [bool]$panicked
        PanicLine    = $panicLine
        # Faults the kernel trapped at CPL=3 and contained. Not a failure --
        # reported so a Ring-3 isolation test can assert it actually exercised
        # the path it meant to.
        ContainedUserFaults = $contained
        # The shell banner is authoritative. The heartbeat is a fallback for
        # configurations where the shell talks to the framebuffer only: by the
        # third beat the scheduler has demonstrably preempted and resumed a
        # task, which is the property we actually care about.
        ReachedShell = [bool](($Text -match $Script:ShellPattern) -or ($Text -match $Script:ThirdBeatPattern))
        Beats        = ([regex]::Matches($Text, $Script:BeatPattern)).Count
    }
}

function Get-SchedulerVerdict {
    <#
    .SYNOPSIS
    Decide whether an observed heartbeat count over a wall-clock window means
    the scheduler is still running.

    .DESCRIPTION
    The `heartbeat` task sleeps `sleep_ticks(100)` against a 100 Hz PIT, so it
    emits one line per second of *interrupt* time. Counting those lines over a
    known wall-clock window measures how many timer interrupts actually
    reached the CPU. A kernel that has masked interrupts, deadlocked inside an
    ISR, or stopped scheduling shows a rate far below 1 Hz while still looking
    "up" to every other check.

    The 0.5 floor is set from measurement, not taste: a healthy image emits
    20/20 beats, and an image under the ELF-loader stall lost 93% of its ticks
    (11 beats across 150s, 0.07 Hz). Anything between those is ambiguous, so
    the threshold sits far from both -- it will not fire on emulator jitter and
    cannot miss a real stall.
    #>
    param(
        [Parameter(Mandatory)][int]$Beats,
        [Parameter(Mandatory)][int]$WindowSeconds
    )

    $floor = [int]($WindowSeconds * 0.5)
    if ($Beats -lt $floor) {
        return [pscustomobject]@{
            Status   = "FAIL"
            Evidence = "scheduler stalled: $Beats heartbeats in ${WindowSeconds}s, expected >= $floor"
        }
    }
    [pscustomobject]@{
        Status   = "PASS"
        Evidence = "$Beats heartbeats in ${WindowSeconds}s"
    }
}

function Invoke-LivenessProbe {
    param([string]$Image, [string]$OutDir, [int]$TimeoutSeconds)

    $probeDir = Join-Path $OutDir "liveness"
    New-Item -ItemType Directory -Force -Path $probeDir | Out-Null
    $probeImage = Join-Path $probeDir "liveness.img"
    Copy-Item -LiteralPath $Image -Destination $probeImage
    $serialLog = Join-Path $probeDir "serial.log"

    $qemuArgs = @(
        "-drive", "format=raw,file=$probeImage",
        "-m", "128M",
        "-display", "none",
        "-no-reboot",
        "-serial", "file:$serialLog"
    )
    # No -WindowStyle: it is Windows-only, and -display none already means
    # there is no window to style.
    $proc = Start-Process -FilePath $Qemu -ArgumentList $qemuArgs -PassThru

    function Read-Serial {
        if (-not (Test-Path -LiteralPath $serialLog)) { return "" }
        $raw = Get-Content -LiteralPath $serialLog -Raw -ErrorAction SilentlyContinue
        if ($raw) { $raw } else { "" }
    }

    try {
        $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
        $state = $null

        while ((Get-Date) -lt $deadline) {
            Start-Sleep -Seconds 2
            $state = Get-SerialState -Text (Read-Serial)
            if ($state.Panicked -or $state.ReachedShell) { break }

            # QEMU exiting before either marker means the kernel triple
            # faulted: the reboot loop is suppressed by -no-reboot, so the
            # process dies instead of spinning. Without this the probe would
            # sit out the whole timeout and misreport a crash as a hang.
            if ($proc.HasExited) {
                return [pscustomobject]@{
                    Status   = "FAIL"
                    Evidence = "QEMU exited during boot (code $($proc.ExitCode)) -- likely triple fault"
                    Dir      = $probeDir
                }
            }
        }
        if (-not $state) { $state = Get-SerialState -Text (Read-Serial) }

        if ($state.Panicked) {
            return [pscustomobject]@{
                Status   = "FAIL"
                Evidence = "kernel panic during boot: $($state.PanicLine)"
                Dir      = $probeDir
            }
        }
        if (-not $state.ReachedShell) {
            return [pscustomobject]@{
                Status   = "FAIL"
                Evidence = "no shell within ${TimeoutSeconds}s (hang or deadlock); last beat #$($state.Beats)"
                Dir      = $probeDir
            }
        }

        # Scheduler check: count heartbeats across a fixed wall-clock window.
        $window = 20
        $before = $state.Beats
        Start-Sleep -Seconds $window
        $after = Get-SerialState -Text (Read-Serial)

        if ($after.Panicked) {
            return [pscustomobject]@{
                Status   = "FAIL"
                Evidence = "kernel panicked while running: $($after.PanicLine)"
                Dir      = $probeDir
            }
        }

        $scheduler = Get-SchedulerVerdict -Beats ($after.Beats - $before) -WindowSeconds $window
        return [pscustomobject]@{
            Status   = $scheduler.Status
            Evidence = if ($scheduler.Status -eq "PASS") {
                "booted to shell, $($scheduler.Evidence), no panic"
            }
            else { $scheduler.Evidence }
            Dir      = $probeDir
        }
    }
    finally {
        if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    }
}

# -------------------------------------------------------------- self-test
#
# Boots nothing. Runs the two classifiers against captured serial logs and
# against the numeric boundary, so a change that quietly breaks failure
# detection is caught by `-SelfTest` in about a second instead of by a green
# run over a broken kernel.
#
# The fixtures are real captures, not hand-written text: an OOM panic from a
# recorded exploit run, a healthy boot, and a boot truncated in the bootloader.

function Invoke-SelfTest {
    $fixtures = Join-Path $ProjectRoot "tests\fixtures\serial"
    $cases = @(
        @{ Name = "healthy boot is not a panic"; File = "boot-healthy.log"; Panicked = $false; ReachedShell = $true }
        @{ Name = "OOM panic is detected"; File = "panic-oom.log"; Panicked = $true; ReachedShell = $true }
        @{ Name = "truncated boot is not a shell"; File = "hang-no-shell.log"; Panicked = $false; ReachedShell = $false }
    )

    $failures = 0
    foreach ($case in $cases) {
        $path = Join-Path $fixtures $case.File
        if (-not (Test-Path -LiteralPath $path)) {
            Write-Host "[FAIL] $($case.Name): fixture missing ($($case.File))" -ForegroundColor Red
            $failures++
            continue
        }
        $state = Get-SerialState -Text (Get-Content -LiteralPath $path -Raw)
        $ok = ($state.Panicked -eq $case.Panicked) -and ($state.ReachedShell -eq $case.ReachedShell)
        if ($ok) {
            Write-Host "[PASS] $($case.Name)" -ForegroundColor Green
        }
        else {
            Write-Host ("[FAIL] {0}: got panicked={1} shell={2}, expected panicked={3} shell={4}" -f `
                    $case.Name, $state.Panicked, $state.ReachedShell, $case.Panicked, $case.ReachedShell) -ForegroundColor Red
            $failures++
        }
    }

    # Ring-3 containment. These two captures differ by one line, and that line
    # is the difference between "the kernel works" and "the kernel died". The
    # text is copied from the format strings in kernel/src/interrupts.rs.
    $contained = @"
EXCEPTION: PAGE FAULT at VirtAddr(0x0)
error_code=PROTECTION_VIOLATION | CAUSED_BY_WRITE | USER_MODE
usermode: page fault trapped safely from CPL=3 at VirtAddr(0x0) (RIP=VirtAddr(0x401000)) -- terminating the offending process, kernel continues
task heartbeat: beat #42
"@
    $state = Get-SerialState -Text $contained
    if ($state.Panicked) {
        Write-Host "[FAIL] contained CPL=3 page fault reported as a kernel death" -ForegroundColor Red
        $failures++
    }
    elseif ($state.ContainedUserFaults -ne 1) {
        Write-Host "[FAIL] contained fault not counted (got $($state.ContainedUserFaults))" -ForegroundColor Red
        $failures++
    }
    else { Write-Host "[PASS] contained CPL=3 fault is not a kernel death" -ForegroundColor Green }

    $ring0 = @"
EXCEPTION: PAGE FAULT at VirtAddr(0xdeadbeef)
error_code=PROTECTION_VIOLATION | CAUSED_BY_WRITE
InterruptStackFrame { instruction_pointer: VirtAddr(0xffff800000201234) }
"@
    $state = Get-SerialState -Text $ring0
    if (-not $state.Panicked) {
        Write-Host "[FAIL] unrecovered Ring-0 page fault not detected" -ForegroundColor Red
        $failures++
    }
    else { Write-Host "[PASS] unrecovered Ring-0 fault detected: $($state.PanicLine)" -ForegroundColor Green }

    # Several user faults plus one kernel fault in the same run: the kernel
    # fault must not be masked by the recoveries around it.
    $mixed = $contained + $contained + $ring0
    $state = Get-SerialState -Text $mixed
    if ($state.Panicked -and $state.ContainedUserFaults -eq 2) {
        Write-Host "[PASS] one Ring-0 fault among two contained ones still detected" -ForegroundColor Green
    }
    else {
        Write-Host ("[FAIL] mixed case: panicked={0} contained={1}, expected True/2" -f `
                $state.Panicked, $state.ContainedUserFaults) -ForegroundColor Red
        $failures++
    }

    # The panic block must yield the cause, not just the banner: an operator
    # reading results.json should see why it died without opening the log.
    $oom = Join-Path $fixtures "panic-oom.log"
    if (Test-Path -LiteralPath $oom) {
        $detail = (Get-SerialState -Text (Get-Content -LiteralPath $oom -Raw)).PanicLine
        if ($detail -match 'memory allocation of \d+ bytes failed') {
            Write-Host "[PASS] panic cause extracted: $detail" -ForegroundColor Green
        }
        else {
            Write-Host "[FAIL] panic cause not extracted, got: '$detail'" -ForegroundColor Red
            $failures++
        }
    }

    # Regression: a multi-digit beat number must still count as "scheduling
    # has happened". The first version of this matched `beat #[3-9]\b`, which
    # reads `beat #97` as never having reached the third beat.
    $late = Get-SerialState -Text "task heartbeat: beat #97"
    if (-not $late.ReachedShell) {
        Write-Host "[FAIL] 'beat #97' not recognised as past the third beat" -ForegroundColor Red
        $failures++
    }
    else { Write-Host "[PASS] multi-digit beat number recognised" -ForegroundColor Green }

    $early = Get-SerialState -Text "task heartbeat: beat #1"
    if ($early.ReachedShell) {
        Write-Host "[FAIL] 'beat #1' treated as a healthy running system" -ForegroundColor Red
        $failures++
    }
    else { Write-Host "[PASS] first beat alone is not enough" -ForegroundColor Green }

    # An empty capture must not read as healthy -- the failure mode where the
    # serial file never appears has to be a FAIL, not a silent pass.
    $empty = Get-SerialState -Text ""
    if ($empty.ReachedShell -or $empty.Panicked) {
        Write-Host "[FAIL] empty capture classified as reaching shell or panicking" -ForegroundColor Red
        $failures++
    }
    else { Write-Host "[PASS] empty capture is neither shell nor panic" -ForegroundColor Green }

    # Scheduler threshold, at and around the boundary.
    $rates = @(
        @{ Beats = 20; Window = 20; Expect = "PASS"; Why = "healthy: measured 20/20 on a good image" }
        @{ Beats = 10; Window = 20; Expect = "PASS"; Why = "at the floor" }
        @{ Beats = 9; Window = 20; Expect = "FAIL"; Why = "just under the floor" }
        @{ Beats = 1; Window = 20; Expect = "FAIL"; Why = "stall: ELF-loader case lost 93% of ticks" }
        @{ Beats = 0; Window = 20; Expect = "FAIL"; Why = "no timer interrupts at all" }
    )
    foreach ($rate in $rates) {
        $got = (Get-SchedulerVerdict -Beats $rate.Beats -WindowSeconds $rate.Window).Status
        if ($got -eq $rate.Expect) {
            Write-Host "[PASS] $($rate.Beats)/$($rate.Window)s -> $got ($($rate.Why))" -ForegroundColor Green
        }
        else {
            Write-Host "[FAIL] $($rate.Beats)/$($rate.Window)s -> $got, expected $($rate.Expect)" -ForegroundColor Red
            $failures++
        }
    }

    # Suite start-failure classification. The last two cases are the important
    # ones: a genuine kernel failure and an unrecognised error must both stay
    # FAIL, or the harness would launder real breakage into a skip.
    $starts = @(
        @{ Text = "The parameter '-WindowStyle' is not supported for the cmdlet 'Start-Process' on this edition of PowerShell."
            Skip = $true; Why = "Windows-only cmdlet parameter"
        }
        @{ Text = "Acceptance must run on the Phase 5 branch or a Phase 6 successor (current: 'main')."
            Skip = $true; Why = "branch precondition"
        }
        @{ Text = "Disk image not found. Run scripts\build.ps1 first."
            Skip = $true; Why = "no image"
        }
        @{ Text = "FAIL: shell did not echo 'ls' within 30s"
            Skip = $false; Why = "a real test failure must stay FAIL"
        }
        @{ Text = "Something nobody has seen before went wrong"
            Skip = $false; Why = "unknown errors stay FAIL, never SKIP"
        }
        @{ Text = "FAIL: expected image not found on disk after write"
            Skip = $false; Why = "an assertion that merely says 'not found' stays FAIL"
        }
        @{ Text = "FAIL: qemu monitor response not found in output"
            Skip = $false; Why = "an assertion mentioning qemu stays FAIL"
        }
        @{ Text = ""; Skip = $false; Why = "empty output is not a start failure" }
    )
    foreach ($start in $starts) {
        $got = [bool](Get-SuiteStartFailure -Text $start.Text)
        if ($got -eq $start.Skip) {
            Write-Host "[PASS] start-failure: $($start.Why)" -ForegroundColor Green
        }
        else {
            Write-Host "[FAIL] start-failure: $($start.Why) -- classified skip=$got" -ForegroundColor Red
            $failures++
        }
    }

    Write-Host ""
    if ($failures -eq 0) {
        Write-Host "self-test: all checks passed" -ForegroundColor Green
        return 0
    }
    Write-Host "self-test: $failures check(s) failed" -ForegroundColor Red
    return 1
}

if ($SelfTest) { exit (Invoke-SelfTest) }

# --------------------------------------------------------------- execution

$started = Get-Date

if (-not $ImagePresent -or -not $QemuPresent) {
    Add-Result -Name "liveness" -Status "SKIP" -Evidence ($Notes -join "; ")
}
else {
    $t0 = Get-Date
    $probe = Invoke-LivenessProbe -Image $Image -OutDir $OutDir -TimeoutSeconds $BootTimeout
    Add-Result -Name "liveness" -Status $probe.Status -Evidence $probe.Evidence `
        -DurationSeconds ([int]((Get-Date) - $t0).TotalSeconds) -Artifacts $probe.Dir
}

if (-not $LivenessOnly) {
    foreach ($entry in $Registry) {
        if ($Suite -and -not ($Suite | Where-Object { $entry.Name -like "*$_*" })) { continue }

        $path = Join-Path $ProjectRoot $entry.Script
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            $status = if ($entry.Required) { "FAIL" } else { "SKIP" }
            Add-Result -Name $entry.Name -Status $status -Evidence "suite script not found: $($entry.Script)"
            continue
        }
        if (-not $ImagePresent -or -not $QemuPresent) {
            Add-Result -Name $entry.Name -Status "SKIP" -Evidence ($Notes -join "; ")
            continue
        }

        $suiteOut = Join-Path $OutDir $entry.Name
        $t0 = Get-Date
        Write-Host "running $($entry.Name)..." -ForegroundColor DarkGray

        # Suites do not share a parameter set -- qemu-smoke-test.ps1 has no
        # param() block at all and discovers its own image. Pass only what a
        # given suite actually declares, so adding a suite never requires
        # editing its signature to fit this harness.
        $accepts = (Get-Command $path).Parameters.Keys
        $splat = @{}
        if ($accepts -contains "Image") { $splat["Image"] = $Image }
        if ($accepts -contains "OutDir") { $splat["OutDir"] = $suiteOut }
        if ($accepts -contains "AllowDirty" -and $AllowDirty) { $splat["AllowDirty"] = $true }

        $suiteLog = Join-Path $OutDir "$($entry.Name).log"
        try {
            & $path @splat 2>&1 | Tee-Object -FilePath $suiteLog | Out-Null
            $code = $LASTEXITCODE
        }
        catch {
            $code = 1
            # Append rather than overwrite: whatever the suite managed to print
            # before it died is the context for why it died, and an earlier
            # version of this discarded it.
            Add-Content -LiteralPath $suiteLog -Value $_.Exception.Message
        }
        $elapsed = [int]((Get-Date) - $t0).TotalSeconds

        # Three sources of truth, most authoritative first.
        #
        # The suites write their verdict to manifest.json, not results.json,
        # and most of them fall off the end without calling exit -- so an exit
        # code of 0 does NOT mean they passed. Reading the manifest is what
        # makes this harness honest; treating exit code as the verdict would
        # report a green run for a suite that recorded a failure.
        $status = $null
        $evidence = ""

        $manifest = Join-Path $suiteOut "manifest.json"
        if (Test-Path -LiteralPath $manifest) {
            try {
                $parsed = Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json
                if ($parsed.PSObject.Properties.Name -contains "verdict") {
                    $verdictText = [string]$parsed.verdict
                    # A suite that declined to run its own checks is a gap in
                    # coverage, not a defect in the kernel: SKIP, never PASS.
                    $status = switch -Regex ($verdictText) {
                        '^PASS$' { "PASS"; break }
                        'SKIP|INCOMPLETE' { "SKIP"; break }
                        default { "FAIL" }
                    }
                    $evidence = "suite verdict: $verdictText"
                }
            }
            catch {
                $evidence = "manifest.json unreadable: $($_.Exception.Message)"
            }
        }

        if (-not $status) {
            $suiteResults = Join-Path $suiteOut "results.json"
            if (Test-Path -LiteralPath $suiteResults) {
                try {
                    $rows = @(Get-Content -LiteralPath $suiteResults -Raw | ConvertFrom-Json)
                    # A row the suite itself marked as skipped is not a
                    # failing check; only outcomes outside PASS/SKIP are.
                    $bad = @($rows | Where-Object {
                            $_.PSObject.Properties.Name -contains "status" -and
                            $_.status -notmatch '^(PASS|SKIP)$'
                        }).Count
                    $status = if ($bad -gt 0) { "FAIL" } elseif ($rows.Count -gt 0) { "PASS" } else { "SKIP" }
                    $evidence = "$($rows.Count) checks, $bad not passing"
                }
                catch {
                    $evidence = "results.json unreadable: $($_.Exception.Message)"
                }
            }
        }

        if (-not $status) {
            # No verdict of any kind. Before calling it a failure, check
            # whether the suite ever started: a suite that bailed out on the
            # host environment says nothing about the kernel, and recording
            # that as FAIL would attribute an operator problem to the code
            # under test.
            $console = ""
            $consolePath = Join-Path $OutDir "$($entry.Name).log"
            if (Test-Path -LiteralPath $consolePath) {
                $console = Get-Content -LiteralPath $consolePath -Raw -ErrorAction SilentlyContinue
            }
            $reason = Get-SuiteStartFailure -Text $console
            if ($reason) {
                $status = "SKIP"
                $evidence = $reason
                $Notes.Add("$($entry.Name): $reason")
            }
            else {
                $status = if ($code -eq 0) { "PASS" } else { "FAIL" }
                $evidence = "no manifest or results; exit code $code"
            }
        }

        $artifacts = if ($accepts -contains "OutDir") { $suiteOut } else { "(suite chose its own output directory)" }
        Add-Result -Name $entry.Name -Status $status -Evidence $evidence `
            -DurationSeconds $elapsed -Artifacts $artifacts
    }
}

# ----------------------------------------------------------------- verdict

$pass = @($Results | Where-Object { $_.status -eq "PASS" }).Count
$fail = @($Results | Where-Object { $_.status -eq "FAIL" }).Count
$skip = @($Results | Where-Object { $_.status -eq "SKIP" }).Count

$verdict =
if ($fail -gt 0) { "FAIL" }
elseif ($pass -eq 0) { "SKIP" }
elseif ($skip -gt 0) { "PARTIAL" }
else { "PASS" }

$totalSeconds = [int]((Get-Date) - $started).TotalSeconds

$summary = [pscustomobject]@{
    verdict         = $verdict
    commit          = $Commit
    image           = $Image
    started_utc     = $started.ToUniversalTime().ToString("o")
    duration_seconds = $totalSeconds
    counts          = [pscustomobject]@{ pass = $pass; fail = $fail; skip = $skip }
    notes           = @($Notes)
    results         = @($Results)
}
$summary | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $ResultsJson -Encoding utf8

[pscustomobject]@{
    suite       = "acceptance"
    commit      = $Commit
    generated   = (Get-Date).ToUniversalTime().ToString("o")
    results     = "results.json"
    report      = "report.txt"
} | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $ManifestJson -Encoding utf8

$report = [System.Text.StringBuilder]::new()
[void]$report.AppendLine("TuwaiqOS acceptance report")
[void]$report.AppendLine("commit   : $Commit")
[void]$report.AppendLine("image    : $Image")
[void]$report.AppendLine("duration : ${totalSeconds}s")
[void]$report.AppendLine("verdict  : $verdict")
[void]$report.AppendLine("")
foreach ($r in $Results) {
    [void]$report.AppendLine(("{0,-6} {1,-16} {2,4}s  {3}" -f $r.status, $r.name, $r.duration_seconds, $r.evidence))
}
if ($Notes.Count -gt 0) {
    [void]$report.AppendLine("")
    foreach ($n in $Notes) { [void]$report.AppendLine("note: $n") }
}
Set-Content -LiteralPath $ReportPath -Value $report.ToString() -Encoding utf8

Write-Host ""
Write-Host ("verdict: {0}   pass {1}  fail {2}  skip {3}   ({4}s)" -f $verdict, $pass, $fail, $skip, $totalSeconds) `
    -ForegroundColor $(switch ($verdict) { "PASS" { "Green" } "FAIL" { "Red" } default { "Yellow" } })
Write-Host "report : $ReportPath"

exit $(switch ($verdict) { "PASS" { 0 } "FAIL" { 1 } "PARTIAL" { 2 } default { 3 } })
