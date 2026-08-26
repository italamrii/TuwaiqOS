# Phase 1-5 acceptance regression for TuwaiqOS.
#
# This harness is intentionally assertion-driven. A command timing out, a
# missing expected marker, a kernel panic, or a skipped critical check makes
# the run fail. Evidence is written below target\phase5-acceptance and is tied
# to both the exact Git commit and the SHA-256 of the clean-built disk image.

[CmdletBinding()]
param(
    [string]$Image,
    [string]$OutDir,
    [int]$MonitorPort = 45751,
    [int]$SerialPort = 45752,
    [switch]$AllowDirty,
    [switch]$SkipExhaustion
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot

if (-not $Image) {
    $Image = Join-Path $ProjectRoot "target\debug\boot-bios-tuwaiqos.img"
}
$Image = [System.IO.Path]::GetFullPath($Image)
if (-not (Test-Path -LiteralPath $Image -PathType Leaf)) {
    throw "Disk image not found at '$Image'. Run scripts\build.ps1 first."
}

$Commit = (& git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $Commit -notmatch '^[0-9a-f]{40}$') {
    throw "Unable to resolve the current Git commit."
}
$Branch = (& git branch --show-current).Trim()
if ($LASTEXITCODE -ne 0 -or
    ($Branch -ne "phase5/userland-desktop-foundation" -and $Branch -notmatch '^phase6/')) {
    throw "Acceptance must run on the Phase 5 branch or a Phase 6 successor (current: '$Branch')."
}
$Dirty = @(& git status --porcelain=v1 --untracked-files=all)
if ($LASTEXITCODE -ne 0) {
    throw "Unable to inspect the Git worktree."
}
if ($Dirty.Count -gt 0 -and -not $AllowDirty) {
    throw "Worktree is dirty; commit the exact acceptance candidate or pass -AllowDirty for development only."
}

$Timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
if (-not $OutDir) {
    $OutDir = Join-Path $ProjectRoot "target\phase5-acceptance\$($Commit.Substring(0, 12))-$Timestamp"
}
$OutDir = [System.IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$QemuCommand = Get-Command qemu-system-x86_64 -ErrorAction SilentlyContinue
if ($QemuCommand) {
    $Qemu = $QemuCommand.Source
} else {
    $Qemu = "C:\Program Files\qemu\qemu-system-x86_64.exe"
}
if (-not (Test-Path -LiteralPath $Qemu -PathType Leaf)) {
    throw "qemu-system-x86_64 was not found in PATH or at '$Qemu'."
}

# -WindowStyle is a Windows-only Start-Process parameter; Unix PowerShell
# rejects it outright. QEMU is already headless (-display none) -- the style
# only hides the extra console window Windows would open for the child.
$HiddenWindowStyle = if ($PSVersionTable.PSVersion.Major -lt 6 -or $IsWindows) {
    @{ WindowStyle = 'Hidden' }
} else {
    @{}
}

$SourceImageHash = (Get-FileHash -LiteralPath $Image -Algorithm SHA256).Hash.ToLowerInvariant()
$TestImage = Join-Path $OutDir "acceptance.img"
Copy-Item -LiteralPath $Image -Destination $TestImage
$TestImageHash = (Get-FileHash -LiteralPath $TestImage -Algorithm SHA256).Hash.ToLowerInvariant()
if ($TestImageHash -ne $SourceImageHash) {
    throw "The acceptance image copy does not match the clean-built source image."
}

$SerialLog = Join-Path $OutDir "serial.log"
$ResultsJson = Join-Path $OutDir "results.json"
$ManifestJson = Join-Path $OutDir "manifest.json"
$Results = [System.Collections.Generic.List[object]]::new()
$SerialText = [System.Text.StringBuilder]::new()
$Proc = $null
$MonitorClient = $null
$SerialClient = $null
$MonitorStream = $null
$SerialStream = $null
$MonitorWriter = $null
$RunError = $null

function Add-Result {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Status,
        [Parameter(Mandatory)][string]$Evidence
    )
    $Results.Add([pscustomobject]@{
        name = $Name
        status = $Status
        evidence = $Evidence
    })
    $color = if ($Status -eq "PASS") { "Green" } elseif ($Status -eq "SKIP") { "Yellow" } else { "Red" }
    Write-Host "[$Status] $Name - $Evidence" -ForegroundColor $color
}

function Pump-Serial {
    param([int]$WaitMilliseconds = 0)
    if ($WaitMilliseconds -gt 0) {
        Start-Sleep -Milliseconds $WaitMilliseconds
    }
    while ($null -ne $SerialStream -and $SerialStream.DataAvailable) {
        $buffer = New-Object byte[] 16384
        $count = $SerialStream.Read($buffer, 0, $buffer.Length)
        if ($count -le 0) {
            break
        }
        [void]$SerialText.Append([System.Text.Encoding]::ASCII.GetString($buffer, 0, $count))
    }
}

function Save-Serial {
    Pump-Serial
    [System.IO.File]::WriteAllText($SerialLog, $SerialText.ToString(), [System.Text.Encoding]::UTF8)
}

function Test-KernelHealth {
    $all = $SerialText.ToString()
    if ($all.Contains("KERNEL PANIC") -or
        $all.Contains("kernel page fault") -or
        $all.Contains("EXCEPTION: DOUBLE FAULT")) {
        throw "Kernel panic/fault detected in the serial log."
    }
    if ($null -ne $Proc -and $Proc.HasExited) {
        throw "QEMU exited unexpectedly with code $($Proc.ExitCode)."
    }
}

function Connect-Tcp {
    param(
        [Parameter(Mandatory)][int]$Port,
        [Parameter(Mandatory)][string]$Name,
        [int]$TimeoutSeconds = 20
    )
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    $lastError = $null
    while ((Get-Date) -lt $deadline) {
        $client = [System.Net.Sockets.TcpClient]::new()
        try {
            $client.Connect("127.0.0.1", $Port)
            return $client
        } catch {
            $lastError = $_
            $client.Dispose()
            Start-Sleep -Milliseconds 200
        }
    }
    throw "Timed out connecting to QEMU $Name port ${Port}: $lastError"
}

function Invoke-Monitor {
    param(
        [Parameter(Mandatory)][string]$Command,
        [int]$SettleMilliseconds = 40
    )
    $MonitorWriter.WriteLine($Command)
    if ($SettleMilliseconds -gt 0) {
        Start-Sleep -Milliseconds $SettleMilliseconds
    }
    while ($MonitorStream.DataAvailable) {
        $discard = New-Object byte[] 4096
        [void]$MonitorStream.Read($discard, 0, $discard.Length)
    }
    Pump-Serial
    Test-KernelHealth
}

function Send-Key {
    param([Parameter(Mandatory)][string]$Key)
    Invoke-Monitor "sendkey $Key" 35
}

function Send-Text {
    param([Parameter(Mandatory)][string]$Text)
    $keyMap = @{
        ' ' = 'spc'
        '_' = 'shift-minus'
        '-' = 'minus'
        '.' = 'dot'
        '/' = 'slash'
    }
    foreach ($character in $Text.ToCharArray()) {
        $value = [string]$character
        if ($keyMap.ContainsKey($value)) {
            Send-Key $keyMap[$value]
        } elseif ($value -cmatch '^[a-z0-9]$') {
            Send-Key $value
        } else {
            throw "No QEMU sendkey mapping is defined for '$value' in '$Text'."
        }
    }
}

function Wait-SerialRegex {
    param(
        [Parameter(Mandatory)][string]$Pattern,
        [int]$StartOffset = 0,
        [int]$TimeoutSeconds = 30
    )
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        Pump-Serial 100
        Test-KernelHealth
        $all = $SerialText.ToString()
        if ($StartOffset -gt $all.Length) {
            $StartOffset = $all.Length
        }
        if ($all.Substring($StartOffset) -match $Pattern) {
            return $Matches[0]
        }
    }
    Save-Serial
    throw "Timed out waiting for serial regex '$Pattern' after offset $StartOffset."
}

function Assert-Regex {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Text,
        [Parameter(Mandatory)][string]$Pattern,
        [string]$Evidence = $Pattern
    )
    if ($Text -notmatch $Pattern) {
        Add-Result $Name "FAIL" "missing regex: $Pattern"
        throw "Acceptance assertion failed: $Name"
    }
    Add-Result $Name "PASS" $Evidence
}

function Assert-NotRegex {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Text,
        [Parameter(Mandatory)][string]$Pattern,
        [string]$Evidence = "regex absent: $Pattern"
    )
    if ($Text -match $Pattern) {
        Add-Result $Name "FAIL" "unexpected regex: $Pattern"
        throw "Acceptance assertion failed: $Name"
    }
    Add-Result $Name "PASS" $Evidence
}

function Invoke-ShellCommand {
    param(
        [Parameter(Mandatory)][string]$Command,
        [int]$TimeoutSeconds = 45,
        [string[]]$Expected = @()
    )
    Pump-Serial
    $offset = $SerialText.Length
    Send-Text $Command
    Send-Key "ret"
    $escaped = [regex]::Escape($Command)
    [void](Wait-SerialRegex "shell: command complete: $escaped(?:\r?\n|$)" $offset $TimeoutSeconds)
    Pump-Serial 100
    $segment = $SerialText.ToString().Substring($offset)
    foreach ($pattern in $Expected) {
        if ($segment -notmatch $pattern) {
            throw "Command '$Command' completed but did not produce expected regex '$pattern'."
        }
    }
    Test-KernelHealth
    return $segment
}

function Save-Screenshot {
    param([Parameter(Mandatory)][string]$Name)
    $path = Join-Path $OutDir "$Name.ppm"
    $qemuPath = $path.Replace('\', '/')
    Invoke-Monitor "screendump $qemuPath" 300
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "QEMU did not create screenshot '$path'."
    }
    return $path
}

try {
    $QemuVersion = (& $Qemu --version | Select-Object -First 1)
    $arguments = @(
        "-drive", "format=raw,file=$TestImage",
        "-m", "128M",
        "-display", "none",
        "-serial", "tcp:127.0.0.1:$SerialPort,server,nowait",
        "-monitor", "tcp:127.0.0.1:$MonitorPort,server,nowait",
        "-no-shutdown"
    )
    $Proc = Start-Process -FilePath $Qemu -ArgumentList $arguments -PassThru @HiddenWindowStyle
    $SerialClient = Connect-Tcp $SerialPort "serial"
    $MonitorClient = Connect-Tcp $MonitorPort "monitor"
    $SerialStream = $SerialClient.GetStream()
    $MonitorStream = $MonitorClient.GetStream()
    $MonitorWriter = [System.IO.StreamWriter]::new($MonitorStream)
    $MonitorWriter.AutoFlush = $true

    [void](Wait-SerialRegex 'task heartbeat: beat #1(?:\r?\n|$)' 0 90)
    Pump-Serial 300
    $boot = $SerialText.ToString()
    Assert-Regex "boot reached kernel" $boot 'TuwaiqOS v0\.5 kernel_main: booting'
    Assert-Regex "paging NX enabled" $boot 'paging: EFER\.NXE enabled and verified'
    Assert-Regex "breakpoint interrupt" $boot 'interrupts: breakpoint self-test OK'
    Assert-Regex "PIT and preemption clock" $boot 'interrupts: IDT/PIC/PIT online, timer at 100 Hz'
    Assert-Regex "PS/2 mouse initialized" $boot 'mouse: PS/2 auxiliary device initialized, streaming enabled at 200 Hz'
    Assert-Regex "mouse IRQ enabled" $boot 'interrupts: mouse IRQ \(IRQ12 via cascade\) unmasked'
    Assert-NotRegex "clean boot has no kernel panic" $boot 'KERNEL PANIC'
    [void](Save-Screenshot "01-boot")

    # Phase 1-3 shell, heap, scheduler, and filesystem regression.
    $segment = Invoke-ShellCommand "memtest" 30 @('Heap allocation test passed\.')
    Assert-Regex "heap allocation regression" $segment 'Heap allocation test passed\.'

    # Exercise the latency-sensitive desktop path early so development runs
    # fail fast before the intentionally expensive physical-exhaustion test.
    $interaction = Invoke-ShellCommand "desktopinteraction" 90 @('desktop interaction: PASS')
    Assert-Regex "live desktop drag" $interaction 'desktop: drag window=[0-9]+.*delta=\(-?[1-9][0-9]*,-?[1-9][0-9]*\)'
    Assert-Regex "live desktop close" $interaction 'desktop: close window=[0-9]+'
    Assert-Regex "live desktop launcher" $interaction 'desktop: launcher window=[0-9]+'
    Assert-Regex "live desktop focus/z-order" $interaction 'desktop: focus window=[0-9]+'
    Assert-Regex "desktop interaction resource cleanup" $interaction 'desktop interaction: PASS exit=Some\(0\)'
    Assert-Regex "mouse event delivery latency" $interaction 'mouse_age_max_ticks=[0-2](?:\D|$)'
    Assert-Regex "mouse/input queue had no loss" $interaction 'dropped=0'

    $segment = Invoke-ShellCommand "run hello" 30 @('Hello from TuwaiqOS')
    Assert-Regex "kernel program loader" $segment 'Hello from TuwaiqOS'

    [void](Invoke-ShellCommand "touch phase5.txt")
    [void](Invoke-ShellCommand "write phase5.txt phase5-persist")
    $segment = Invoke-ShellCommand "cat phase5.txt" 30 @('phase5-persist')
    Assert-Regex "filesystem create/write/read" $segment 'phase5-persist'

    # Ring 3 ABI, ELF loading, isolation, and fault recovery.
    $segment = Invoke-ShellCommand "runelf hello" 45 @('hello: Ring 3 ELF process alive', 'process: result .*exit_code=0')
    Assert-Regex "ELF Ring 3 process" $segment 'hello: Ring 3 ELF process alive'
    Assert-Regex "basic syscall ABI" $segment 'process: result .*exit_code=0'

    $segment = Invoke-ShellCommand "isolate" 60 @('distinct PML4 physical addresses', 'exit_code=0')
    Assert-Regex "process address-space isolation" $segment 'distinct PML4 physical addresses'
    Assert-Regex "concurrent preempted Ring 3 processes" $segment 'Both finished:[\s\S]*exit_code=0[\s\S]*exit_code=0'

    foreach ($fault in @('bad_kernel', 'bad_unmapped', 'bad_privileged', 'bad_ud2', 'bad_divzero')) {
        $segment = Invoke-ShellCommand "isolate $fault" 60 @('distinct PML4 physical addresses', 'Both finished:')
        Assert-Regex "Ring 3 fault isolation: $fault" $segment 'usermode: (?:page fault|privileged instruction|invalid opcode|divide error) trapped safely from CPL=3[\s\S]*Both finished:'
    }

    $segment = Invoke-ShellCommand "spawnfail 20" 60 @('bump cursor unchanged', 'unexpectedly loaded: 0')
    Assert-Regex "failed ELF load frame rollback" $segment 'bump cursor unchanged -- every failed spawn'

    $segment = Invoke-ShellCommand "reap 20" 90 @('task count returned to baseline', 'frame bump cursor unchanged')
    Assert-Regex "TCB and kernel-stack reaping" $segment 'task count returned to baseline -- no leaked TCBs'
    Assert-Regex "address-space frame reaping" $segment 'frame bump cursor unchanged -- address spaces fully reclaimed'
    $segment = Invoke-ShellCommand "autoreap 5" 90 @('automatic reap: PASS')
    Assert-Regex "automatic grace-period reaping" $segment 'automatic reap: PASS count=5'
    $segment = Invoke-ShellCommand "killreap 20" 90 @('kill reap: PASS')
    Assert-Regex "explicit kill cleanup and kernel-stack reuse" $segment 'kill reap: PASS count=20'

    # Pointer, display, input, and anonymous-memory security tests. Each ELF
    # exits zero only after all of its returning negative assertions pass.
    foreach ($program in @('bad_syscall', 'bad_pointer', 'bad_display', 'bad_input', 'bad_mmap', 'bad_munmap')) {
        $segment = Invoke-ShellCommand "runelf $program" 90 @('process: result .*exit_code=0')
        Assert-Regex "security ELF: $program" $segment 'process: result .*exit_code=0'
        Assert-NotRegex "security ELF has no unexpected branch: $program" $segment 'UNEXPECTED'
    }
    if (-not $SkipExhaustion) {
        $exhaustionWatch = [System.Diagnostics.Stopwatch]::StartNew()
        $segment = Invoke-ShellCommand "runelf mmap_exhaustion" 180 @('mmap exhaustion cleanup: PASS')
        $exhaustionWatch.Stop()
        Assert-Regex "mmap exhaustion rollback and frame reuse" $segment 'mmap exhaustion cleanup: PASS exit_code=0' "64 MiB allocation, bounded rejection, rollback, unmap, and reuse completed in $($exhaustionWatch.ElapsedMilliseconds) ms"
        Assert-Regex "timer progressed during large mmap" $segment 'timer advanced during 64 MiB mmap -- OK'
        Assert-Regex "timer progressed during large munmap" $segment 'timer advanced during 64 MiB munmap -- OK'
        $vmBatchMatch = [regex]::Match($segment, 'vm batch telemetry: count=([0-9]+).*average_cycles=([0-9]+).*max_cycles=([0-9]+).*max_kind=([0-9]+).*cycles_per_tick=([0-9]+).*max_milli_ticks=([0-9]+).*pages_per_batch=4')
        if (-not $vmBatchMatch.Success) {
            throw "VM batch telemetry was missing or malformed."
        }
        $averageVmCycles = [uint64]$vmBatchMatch.Groups[2].Value
        $maxVmKind = [uint32]$vmBatchMatch.Groups[4].Value
        $cyclesPerTick = [uint64]$vmBatchMatch.Groups[5].Value
        $maxVmMilliTicks = [uint64]$vmBatchMatch.Groups[6].Value
        $averageVmMilliTicks = [uint64](($averageVmCycles * 1000) / $cyclesPerTick)
        if ($averageVmMilliTicks -gt 100) {
            throw "Average VM critical section exceeded 1 ms: $averageVmMilliTicks milli-ticks."
        }
        if ($maxVmMilliTicks -gt 10000) {
            throw "VM critical-section kind $maxVmKind exceeded 100 ms: $maxVmMilliTicks milli-ticks."
        }
        Add-Result "bounded VM critical sections" "PASS" "average=$averageVmMilliTicks milli-ticks; max=$maxVmMilliTicks milli-ticks kind=$maxVmKind; limits=100/10000"
        $segment = Invoke-ShellCommand "runelf mmap_partial_failure" 60 @('mmap partial rollback: PASS')
        Assert-Regex "post-mutation mmap rollback" $segment 'mmap_partial_failure: injected partial map rejected -- OK[\s\S]*rollback, same-address retry, zero-fill, and cleanup -- OK'
        Assert-Regex "post-mutation mmap exact ownership rollback" $segment 'mmap: rollback frames before=([0-9]+) after=\1'
        Assert-Regex "post-mutation mmap resource baseline" $segment 'mmap partial rollback: PASS exit_code=0 live_before=([0-9]+) live_after=\1'
    } elseif (-not $AllowDirty) {
        throw "-SkipExhaustion is permitted only for dirty development runs."
    } else {
        Add-Result "mmap exhaustion rollback and frame reuse" "SKIP" "dirty development run only; final acceptance cannot skip this test"
    }

    # These three processes must be terminated by real hardware permissions;
    # a normal exit is a failure for the test binary by construction.
    foreach ($program in @('mmap_ro_fault', 'mmap_nx_fault', 'post_unmap_fault')) {
        $segment = Invoke-ShellCommand "runelf $program" 60 @('usermode: page fault trapped safely from CPL=3')
        Assert-Regex "hardware memory protection: $program" $segment 'usermode: page fault trapped safely from CPL=3'
        Assert-NotRegex "fault test did not reach failure exit: $program" $segment 'UNEXPECTED'
    }

    $segment = Invoke-ShellCommand "mousetest" 30 @('mousetest: PASS')
    Assert-Regex "mouse packet decoder" $segment 'mousetest: PASS resync signed-motion y-inversion clamp overflow button-edges'
    $segment = Invoke-ShellCommand "desktoptest" 30 @('desktoptest: PASS')
    Assert-Regex "window drag/close model" $segment 'desktoptest: PASS drag close z-order slot-reuse'

    # Host keyboard input while the desktop owns foreground must never be
    # replayed as privileged shell commands after normal exit.
    Pump-Serial
    $desktopOffset = $SerialText.Length
    Send-Text "desktop"
    Send-Key "ret"
    [void](Wait-SerialRegex 'desktop: started(?:\r?\n|$)' $desktopOffset 60)
    [void](Wait-SerialRegex 'desktop: first frame presented(?:\r?\n|$)' $desktopOffset 60)
    [void](Save-Screenshot "02-desktop-start")
    Send-Text "reboot"
    Send-Key "ret"
    Send-Text "kill 1"
    Send-Key "ret"
    [void](Save-Screenshot "03-desktop-keyboard-owned")
    Send-Key "esc"
    [void](Wait-SerialRegex 'shell: command complete: desktop(?:\r?\n|$)' $desktopOffset 60)
    Pump-Serial 300
    $desktopSegment = $SerialText.ToString().Substring($desktopOffset)
    Assert-Regex "desktop foreground acquired" $desktopSegment 'input: foreground acquire owner=desktop pid=[0-9]+'
    Assert-Regex "desktop normal exit" $desktopSegment 'desktop: normal exit requested[\s\S]*exit_code=0'
    Assert-Regex "desktop foreground released" $desktopSegment 'input: foreground release owner=shell former_pid=[0-9]+'
    Assert-NotRegex "desktop command did not replay to shell" $desktopSegment 'shell: command begin: reboot'
    Assert-NotRegex "desktop kill-looking command did not replay" $desktopSegment 'shell: command begin: kill 1'
    Assert-NotRegex "desktop command did not reboot kernel" $desktopSegment 'kernel_main: booting'

    $segment = Invoke-ShellCommand "inputstats" 30 @('input: telemetry label=manual')
    Assert-Regex "foreground input queue telemetry" $segment 'input: telemetry label=manual depth=0 max_depth=[0-9]+ coalesced_moves=[0-9]+ dropped=0'
    Assert-Regex "display present cost telemetry" $segment 'display: telemetry presents=[1-9][0-9]* total_cycles=[1-9][0-9]* average_cycles=[1-9][0-9]* max_cycles=[1-9][0-9]*'
    $displayMatch = [regex]::Match($segment, 'display: telemetry .*max_cycles=([0-9]+).*cycles_per_tick=([0-9]+).*max_milli_ticks=([0-9]+)')
    if (-not $displayMatch.Success) {
        throw "Display present timing telemetry was missing or malformed."
    }
    $maxDisplayMilliTicks = [uint64]$displayMatch.Groups[3].Value
    if ($maxDisplayMilliTicks -gt 5000) {
        throw "A display present exceeded one 50 ms scheduler quantum: $maxDisplayMilliTicks milli-ticks."
    }
    Add-Result "bounded display present critical section" "PASS" "max=$maxDisplayMilliTicks milli-ticks; limit=5000 milli-ticks (one 50 ms quantum)"
    Assert-Regex "mouse IRQ packet telemetry" $segment 'mouse: telemetry initialized=true raw_bytes=[1-9][0-9]* packets=[1-9][0-9]*'

    # Run another Ring 3 process concurrently with the desktop and require its
    # completion marker before the desktop exits.
    Pump-Serial
    $peerOffset = $SerialText.Length
    Send-Text "desktoppeer"
    Send-Key "ret"
    [void](Wait-SerialRegex 'desktop: first frame presented(?:\r?\n|$)' $peerOffset 60)
    [void](Wait-SerialRegex 'desktop_peer: started' $peerOffset 60)
    [void](Wait-SerialRegex 'desktop_peer: completed' $peerOffset 60)
    Send-Key "esc"
    [void](Wait-SerialRegex 'shell: command complete: desktoppeer(?:\r?\n|$)' $peerOffset 60)
    $peerSegment = $SerialText.ToString().Substring($peerOffset)
    Assert-Regex "desktop plus concurrent Ring 3 peer" $peerSegment 'desktop_peer: started[\s\S]*desktop_peer: completed[\s\S]*process: result .*exit_code=0'
    $segment = Invoke-ShellCommand "desktopfaultpeer" 90 @('desktop fault peer: PASS')
    Assert-Regex "faulting peer cannot kill desktop/kernel" $segment 'desktop fault peer: PASS peer_exit=Some\(132\) desktop_survived=true desktop_exit=Some\(0\)'
    $segment = Invoke-ShellCommand "desktopkilltest" 90 @('desktop kill: PASS')
    Assert-Regex "forced desktop failure recovers shell/resources" $segment 'desktop kill: PASS'

    # Twenty start/run/normal-exit/reap cycles. The shell command compares
    # task count, live frames, frame bump cursor, and kernel heap usage against
    # a warmed baseline and fails its own marker on any leak.
    $segment = Invoke-ShellCommand "desktopcycle 20" 180 @('desktop lifecycle: PASS', 'cycles=20')
    Assert-Regex "20-cycle desktop restart" $segment 'desktop lifecycle: PASS.*cycles=20'
    Assert-Regex "desktop lifecycle TCB baseline" $segment 'tasks before=([0-9]+) after=\1'
    Assert-Regex "desktop lifecycle frame baseline" $segment 'live_frames before=([0-9]+) after=\1'
    Assert-Regex "desktop lifecycle kernel heap/stack baseline" $segment 'heap_used before=([0-9]+) after=\1'

    # Responsiveness telemetry is gathered after live mouse bursts. This is a
    # quantitative bound, while coalescing and dirty-only redraw leave the PIT
    # scheduler quantum unchanged.
    # Scheduler fairness must remain observable after all graphical activity.
    Pump-Serial
    $fairnessOffset = $SerialText.Length
    [void](Wait-SerialRegex 'task heartbeat: beat #[0-9]+' $fairnessOffset 5)
    Pump-Serial
    $firstFairness = [regex]::Matches($SerialText.ToString().Substring($fairnessOffset), 'beat #([0-9]+)') | Select-Object -First 1
    $nextOffset = $SerialText.Length
    [void](Wait-SerialRegex 'task heartbeat: beat #[0-9]+' $nextOffset 5)
    Pump-Serial
    $secondFairness = [regex]::Matches($SerialText.ToString().Substring($nextOffset), 'beat #([0-9]+)') | Select-Object -First 1
    if (-not $firstFairness -or -not $secondFairness -or [int]$secondFairness.Groups[1].Value -le [int]$firstFairness.Groups[1].Value) {
        throw "Heartbeat did not advance after desktop/input stress."
    }
    Add-Result "scheduler fairness after graphics" "PASS" "heartbeat advanced from $($firstFairness.Groups[1].Value) to $($secondFairness.Groups[1].Value)"

    # Reboot the same tested disk and prove filesystem persistence plus the
    # ability to launch another Ring 3 process after reset. Reboot is never
    # allowed to degrade into a skipped check.
    Pump-Serial
    $rebootOffset = $SerialText.Length
    Send-Text "reboot"
    Send-Key "ret"
    [void](Wait-SerialRegex 'TuwaiqOS v0\.5 kernel_main: booting' $rebootOffset 60)
    [void](Wait-SerialRegex 'task heartbeat: beat #1(?:\r?\n|$)' $rebootOffset 90)
    $rebootSegment = $SerialText.ToString().Substring($rebootOffset)
    Assert-Regex "machine reboot completed" $rebootSegment 'TuwaiqOS v0\.5 kernel_main: booting'
    $segment = Invoke-ShellCommand "cat phase5.txt" 30 @('phase5-persist')
    Assert-Regex "filesystem persistence after reboot" $segment 'phase5-persist'
    $segment = Invoke-ShellCommand "runelf hello" 45 @('process: result .*exit_code=0')
    Assert-Regex "post-reboot ELF/syscall regression" $segment 'process: result .*exit_code=0'
    [void](Save-Screenshot "05-post-reboot")

    # A second boot with the emulated 8042 removed proves mouse discovery is
    # bounded and fail-closed: boot, PIT, and scheduler must continue, while
    # IRQ12 stays masked and no false initialized state is reported.
    $MonitorWriter.WriteLine("quit")
    $MonitorWriter.Dispose()
    $MonitorWriter = $null
    $MonitorStream.Dispose()
    $MonitorStream = $null
    $SerialStream.Dispose()
    $SerialStream = $null
    $MonitorClient.Dispose()
    $MonitorClient = $null
    $SerialClient.Dispose()
    $SerialClient = $null
    if (-not $Proc.WaitForExit(5000)) {
        throw "Primary QEMU did not exit before the mouse-absent boot."
    }

    [void]$SerialText.Append("`r`n=== mouse-absent boot (pc,i8042=off) ===`r`n")
    $absentOffset = $SerialText.Length
    $absentArguments = @(
        "-machine", "pc,i8042=off",
        "-drive", "format=raw,file=$TestImage",
        "-m", "128M",
        "-display", "none",
        "-serial", "tcp:127.0.0.1:$SerialPort,server,nowait",
        "-monitor", "tcp:127.0.0.1:$MonitorPort,server,nowait",
        "-no-shutdown"
    )
    $Proc = Start-Process -FilePath $Qemu -ArgumentList $absentArguments -PassThru @HiddenWindowStyle
    $SerialClient = Connect-Tcp $SerialPort "mouse-absent serial"
    $MonitorClient = Connect-Tcp $MonitorPort "mouse-absent monitor"
    $SerialStream = $SerialClient.GetStream()
    $MonitorStream = $MonitorClient.GetStream()
    $MonitorWriter = [System.IO.StreamWriter]::new($MonitorStream)
    $MonitorWriter.AutoFlush = $true
    [void](Wait-SerialRegex 'task heartbeat: beat #1(?:\r?\n|$)' $absentOffset 90)
    Pump-Serial 200
    $absentSegment = $SerialText.ToString().Substring($absentOffset)
    Assert-Regex "mouse-absent bounded initialization" $absentSegment 'mouse: (?:PS/2 controller unavailable|PS/2 controller timed out|auxiliary device initialization failed)'
    Assert-Regex "mouse-absent IRQ remains masked" $absentSegment 'mouse: IRQ12 remains masked; boot continues without mouse input'
    Assert-NotRegex "mouse-absent has no false initialized state" $absentSegment 'mouse: PS/2 auxiliary device initialized'
    Assert-Regex "mouse-absent scheduler remains live" $absentSegment 'task heartbeat: beat #1'

    Test-KernelHealth
    if ($SkipExhaustion) {
        Add-Result "complete Phase 1-5 run" "SKIP" "development run omitted the mandatory exhaustion test"
    } else {
        Add-Result "complete Phase 1-5 run" "PASS" "all critical assertions completed; no panic and no skipped test"
    }
} catch {
    $RunError = $_
    Add-Result "acceptance run" "FAIL" $_.Exception.Message
} finally {
    try { Save-Serial } catch {}
    if ($null -ne $MonitorWriter) {
        try { $MonitorWriter.WriteLine("quit") } catch {}
        try { $MonitorWriter.Dispose() } catch {}
    }
    foreach ($resource in @($MonitorStream, $SerialStream, $MonitorClient, $SerialClient)) {
        if ($null -ne $resource) {
            try { $resource.Dispose() } catch {}
        }
    }
    if ($null -ne $Proc -and -not $Proc.HasExited) {
        try {
            if (-not $Proc.WaitForExit(3000)) {
                Stop-Process -Id $Proc.Id -Force -ErrorAction Stop
            }
        } catch {}
    }

    $Results | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $ResultsJson -Encoding UTF8
    $FinalImageHash = (Get-FileHash -LiteralPath $TestImage -Algorithm SHA256).Hash.ToLowerInvariant()
    $manifest = [ordered]@{
        schema = 1
        started_at = $Timestamp
        completed_at = (Get-Date).ToString("o")
        git_commit = $Commit
        git_branch = $Branch
        dirty_development_run = [bool]($Dirty.Count -gt 0)
        skipped_exhaustion = [bool]$SkipExhaustion
        source_image = $Image
        source_image_sha256 = $SourceImageHash
        tested_image = $TestImage
        tested_image_sha256_before = $TestImageHash
        tested_image_sha256_after = $FinalImageHash
        memory_mib = 128
        qemu = $Qemu
        qemu_version = $QemuVersion
        serial_log = $SerialLog
        results = $ResultsJson
        verdict = if ($null -ne $RunError) { "FAIL" } elseif ($SkipExhaustion) { "DEVELOPMENT-SKIPPED" } else { "PASS" }
    }
    $manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $ManifestJson -Encoding UTF8
}

if ($null -ne $RunError) {
    Write-Host "Acceptance failed. Evidence: $OutDir" -ForegroundColor Red
    throw $RunError
}

if ($SkipExhaustion) {
    Write-Host "Development run completed with a mandatory test skipped. Evidence: $OutDir" -ForegroundColor Yellow
} else {
    Write-Host "Acceptance passed. Evidence: $OutDir" -ForegroundColor Green
}
Write-Host "Commit: $Commit"
Write-Host "Clean-built image SHA-256: $SourceImageHash"
