# Hardware degradation suite
#
# Boots TuwaiqOS on deliberately incomplete machines and checks that missing or
# unsupported hardware degrades instead of hanging
#
# For each scenario the suite asks three things
#   1 does the kernel still reach a usable state
#   2 does it say so, by naming the missing device on serial
#   3 do operations that need the missing device fail cleanly rather than hang
#
# Scenarios that remove the PS/2 controller cannot be driven from the keyboard,
# so those are observed from serial only and marked non interactive
#
# Verdicts
#   PASS     every scenario degraded as expected
#   PARTIAL  at least one scenario passed and at least one was skipped
#   FAIL     any scenario hung, panicked or lost a device silently
#   SKIP     nothing could run

[CmdletBinding()]
param(
    [string]$Image,
    [string]$OutDir,
    [string[]]$Scenario,
    [int]$MonitorPort = 45810,
    [int]$BootTimeout = 90,
    [switch]$AllowDirty
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot

# --------------------------------------------------------------- scenarios
#
# Expect is a regex the serial capture must match for the scenario to count as
# reported rather than silently degraded
# Probe commands run only where a keyboard exists
# MustNotMatch guards against a device quietly staying alive when it was removed

$Scenarios = @(
    [pscustomobject]@{
        Name        = "baseline"
        Why         = "reference machine with every device present"
        QemuArgs    = @()
        Interactive = $true
        Expect      = 'vfs: mounted TuwaiqFS v3 at /'
        Probe       = @("sysinfo", "mounts", "ls /")
        ProbeExpect = '/  TuwaiqFS v3  rw'
    }
    [pscustomobject]@{
        Name        = "no-ps2"
        Why         = "PS/2 controller absent so keyboard and mouse cannot initialise"
        QemuArgs    = @("-machine", "pc,i8042=off")
        Interactive = $false
        Expect      = 'mouse: PS/2 controller unavailable|IRQ12 remains masked'
        Probe       = @()
        ProbeExpect = ""
    }
    [pscustomobject]@{
        Name        = "no-legacy-ata"
        Why         = "storage moved off the legacy ATA ports so the disk driver finds nothing"
        QemuArgs    = @("-machine", "q35")
        Interactive = $true
        Expect      = 'vfs: TuwaiqFS unavailable; entering read-only recovery mode: ata error'
        Probe       = @("sysinfo", "mounts", "ls /", "uptime")
        # Matches only the tail of the message on purpose
        # The shell builds this line as a print of the prefix followed by a
        # println of the reason, and the serial lock is released between the
        # two, so another task can and does write into the gap
        #
        #   Filesystem error: task heartbeat: beat #13
        #   no VFS mount for path
        #
        # Anchoring on the whole sentence fails intermittently for reasons
        # that have nothing to do with storage
        ProbeExpect = 'no VFS mount for path'
    }
    [pscustomobject]@{
        Name        = "no-ps2-no-ata"
        Why         = "input and storage both missing at once"
        QemuArgs    = @("-machine", "q35,i8042=off")
        Interactive = $false
        Expect      = 'mouse: PS/2 controller unavailable'
        Probe       = @()
        ProbeExpect = ""
    }
    [pscustomobject]@{
        Name        = "unsupported-devices"
        Why         = "hardware the kernel has no driver for is present on the bus"
        QemuArgs    = @("-device", "e1000", "-device", "intel-hda")
        Interactive = $true
        Expect      = 'vfs: mounted TuwaiqFS v3 at /'
        Probe       = @("sysinfo", "mounts", "ls /")
        ProbeExpect = '/  TuwaiqFS v3  rw'
    }
    [pscustomobject]@{
        Name        = "no-display"
        Why         = "no display adapter at all"
        QemuArgs    = @("-vga", "none")
        Interactive = $false
        Expect      = 'kernel_main: booting'
        Probe       = @()
        ProbeExpect = ""
    }
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

$Dirty = @(& git status --porcelain=v1 --untracked-files=all)
if ($Dirty.Count -gt 0 -and -not $AllowDirty) {
    throw "Worktree is dirty; commit the candidate or pass -AllowDirty for development"
}

$Commit = (& git rev-parse HEAD).Trim()
$Timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
if (-not $OutDir) {
    $OutDir = Join-Path $ProjectRoot "target\degradation\$($Commit.Substring(0, 12))-$Timestamp"
}
$OutDir = [System.IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

Write-Host "=== TuwaiqOS hardware degradation ===" -ForegroundColor Cyan
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

# ------------------------------------------------------------- typing over
# the QEMU monitor
#
# Only the characters the shell commands actually use are mapped
# An unmappable character is a bug in a scenario definition rather than
# something to skip silently

$KeyMap = @{
    ' ' = 'spc'; '/' = 'slash'; '.' = 'dot'; '-' = 'minus'; '_' = 'shift-minus'
}

function Send-Monitor {
    param($Stream, [string]$Command, [double]$Pause = 0.05)
    $bytes = [System.Text.Encoding]::ASCII.GetBytes($Command + "`n")
    $Stream.Write($bytes, 0, $bytes.Length)
    $Stream.Flush()
    Start-Sleep -Seconds $Pause
    $buffer = New-Object byte[] 65536
    while ($Stream.DataAvailable) { [void]$Stream.Read($buffer, 0, $buffer.Length) }
}

function Send-Line {
    param($Stream, [string]$Line, [double]$Settle = 3.0)
    foreach ($ch in $Line.ToCharArray()) {
        # Cast to string before the lookup
        # A [char] key never matches the [string] keys in the table and the
        # miss lands in the throw below rather than anywhere obvious
        $c = [string]$ch
        $key =
        if ($c -cmatch '^[a-z0-9]$') { $c }
        elseif ($c -cmatch '^[A-Z]$') { "shift-$($c.ToLower())" }
        elseif ($KeyMap.ContainsKey($c)) { $KeyMap[$c] }
        else { throw "no key mapping for '$c' in '$Line'" }
        Send-Monitor -Stream $Stream -Command "sendkey $key" -Pause 0.05
    }
    Send-Monitor -Stream $Stream -Command "sendkey ret" -Pause $Settle
}

# ---------------------------------------------------------------- one scenario

function Invoke-Scenario {
    param([Parameter(Mandatory)]$Case, [Parameter(Mandatory)][string]$OutDir, [int]$Port)

    $dir = Join-Path $OutDir $Case.Name
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $disk = Join-Path $dir "disk.img"
    Copy-Item -LiteralPath $Image -Destination $disk
    $serial = Join-Path $dir "serial.log"

    $args = @(
        "-drive", "format=raw,file=$disk",
        "-m", "128M",
        "-display", "none",
        "-no-reboot",
        "-serial", "file:$serial"
    )
    if ($Case.Interactive) { $args += @("-monitor", "telnet:127.0.0.1:$Port,server,nowait") }
    $args += $Case.QemuArgs

    $proc = Start-Process -FilePath $Qemu -ArgumentList $args -PassThru
    $client = $null
    try {
        # Wait for the kernel to announce itself or for the timeout to expire
        # A scenario that never prints anything is the interesting case, so the
        # loop has to run to the end rather than give up early
        $deadline = (Get-Date).AddSeconds($BootTimeout)
        $reached = $false
        while ((Get-Date) -lt $deadline) {
            Start-Sleep -Seconds 3
            if (Test-Path -LiteralPath $serial) {
                $text = Get-Content -LiteralPath $serial -Raw -ErrorAction SilentlyContinue
                if ($text -and $text -match 'beat #(?:[3-9]|\d{2,})\b') { $reached = $true; break }
                if ($text -and $text -match 'KERNEL PANIC|EXCEPTION: DOUBLE FAULT') { break }
            }
            if ($proc.HasExited) { break }
        }

        $probeLog = ""
        if ($reached -and $Case.Interactive -and $Case.Probe.Count -gt 0) {
            $tcp = [System.Net.Sockets.TcpClient]::new("127.0.0.1", $Port)
            $client = $tcp
            $stream = $tcp.GetStream()
            Start-Sleep -Seconds 1
            Send-Monitor -Stream $stream -Command "" -Pause 0.5
            foreach ($command in $Case.Probe) {
                Send-Line -Stream $stream -Line $command
            }
            Start-Sleep -Seconds 2
            $probeLog = "ran $($Case.Probe.Count) shell commands"
        }

        $text = if (Test-Path -LiteralPath $serial) {
            Get-Content -LiteralPath $serial -Raw -ErrorAction SilentlyContinue
        }
        else { "" }
        if (-not $text) { $text = "" }

        return [pscustomobject]@{
            Text     = $text
            Reached  = $reached
            Bytes    = $text.Length
            Beats    = ([regex]::Matches($text, 'beat #')).Count
            Panicked = [bool]($text -match 'KERNEL PANIC|EXCEPTION: DOUBLE FAULT')
            Probed   = $probeLog
            Dir      = $dir
        }
    }
    finally {
        if ($client) { $client.Close() }
        if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    }
}

# --------------------------------------------------------------- execution

$started = Get-Date
$port = $MonitorPort

foreach ($case in $Scenarios) {
    if ($Scenario -and -not ($Scenario | Where-Object { $case.Name -like "*$_*" })) { continue }

    if (-not $ImagePresent -or -not $QemuPresent) {
        Add-Result -Name $case.Name -Status "SKIP" -Evidence ($Notes -join "; ")
        continue
    }

    Write-Host "running $($case.Name) -- $($case.Why)" -ForegroundColor DarkGray
    $t0 = Get-Date
    # One scenario blowing up must not take the rest of the matrix with it
    # The remaining rows are still evidence and a suite that aborts halfway
    # reports far less than one that finishes and marks the broken row
    try {
        $run = Invoke-Scenario -Case $case -OutDir $OutDir -Port $port
    }
    catch {
        $port++
        Add-Result -Name $case.Name -Status "FAIL" `
            -Evidence "harness error: $($_.Exception.Message)" `
            -DurationSeconds ([int]((Get-Date) - $t0).TotalSeconds)
        continue
    }
    $port++
    $elapsed = [int]((Get-Date) - $t0).TotalSeconds

    # Order matters here
    # A panic is always a failure
    # Total silence is a failure even though nothing crashed, because a machine
    # that produces no output cannot be diagnosed by an operator
    # Reaching a running state but never naming the missing device is also a
    # failure, since silent degradation is what this suite exists to catch
    $status = "PASS"
    $evidence = ""

    if ($run.Panicked) {
        $status = "FAIL"
        $evidence = "kernel panicked"
    }
    elseif ($run.Bytes -eq 0) {
        $status = "FAIL"
        $evidence = "no serial output at all within ${BootTimeout}s -- silent hang with nothing to diagnose"
    }
    elseif (-not $run.Reached) {
        $status = "FAIL"
        $evidence = "kernel never reached a scheduling state; $($run.Beats) heartbeats in $($run.Bytes) bytes of output"
    }
    elseif ($run.Text -notmatch $case.Expect) {
        $status = "FAIL"
        $evidence = "reached shell but never reported the degradation; expected /$($case.Expect)/"
    }
    elseif ($case.ProbeExpect -and $run.Probed -and $run.Text -notmatch $case.ProbeExpect) {
        $status = "FAIL"
        $evidence = "shell did not answer as expected; wanted /$($case.ProbeExpect)/"
    }
    else {
        $matched = [regex]::Match($run.Text, $case.Expect).Value
        $evidence = "degraded and reported it: '$matched'"
        if ($run.Probed) { $evidence += "; $($run.Probed)" }
        $evidence += "; $($run.Beats) heartbeats"
    }

    Add-Result -Name $case.Name -Status $status -Evidence $evidence `
        -DurationSeconds $elapsed -Artifacts $run.Dir
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

[pscustomobject]@{
    verdict          = $verdict
    commit           = $Commit
    image            = $Image
    started_utc      = $started.ToUniversalTime().ToString("o")
    duration_seconds = $totalSeconds
    counts           = [pscustomobject]@{ pass = $pass; fail = $fail; skip = $skip }
    notes            = @($Notes)
    results          = @($Results)
} | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $OutDir "results.json") -Encoding utf8

[pscustomobject]@{
    suite     = "degradation"
    commit    = $Commit
    generated = (Get-Date).ToUniversalTime().ToString("o")
    verdict   = $verdict
    results   = "results.json"
} | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $OutDir "manifest.json") -Encoding utf8

$report = [System.Text.StringBuilder]::new()
[void]$report.AppendLine("TuwaiqOS hardware degradation report")
[void]$report.AppendLine("commit   : $Commit")
[void]$report.AppendLine("duration : ${totalSeconds}s")
[void]$report.AppendLine("verdict  : $verdict")
[void]$report.AppendLine("")
foreach ($r in $Results) {
    [void]$report.AppendLine(("{0,-6} {1,-22} {2,4}s  {3}" -f $r.status, $r.name, $r.duration_seconds, $r.evidence))
}
Set-Content -LiteralPath (Join-Path $OutDir "report.txt") -Value $report.ToString() -Encoding utf8

Write-Host ""
Write-Host ("verdict: {0}   pass {1}  fail {2}  skip {3}   ({4}s)" -f $verdict, $pass, $fail, $skip, $totalSeconds) `
    -ForegroundColor $(switch ($verdict) { "PASS" { "Green" } "FAIL" { "Red" } default { "Yellow" } })

exit $(switch ($verdict) { "PASS" { 0 } "FAIL" { 1 } "PARTIAL" { 2 } default { 3 } })
