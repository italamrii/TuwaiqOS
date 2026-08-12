# Crash and relaunch
#
# Kills the same Ring 3 program the same way over and over and checks that the
# kernel and the shell are unchanged by it
#
# The existing coverage takes each fault once See docs/ring3-fault-coverage.md
# for where every requirement is already met What is not covered anywhere is
# repetition The first fault of a kind exercises the handler The twentieth
# exercises whether anything the handler leaves behind accumulates
#
# Three things are asserted after every single crash
#   the offending process died with the exit code its fault implies
#   the shell answered the next command
#   the kernel never panicked
#
# and once at the end
#   the frame bump cursor settled rather than climbing per crash
#
# Verdicts
#   PASS  every crash contained and resources settled
#   FAIL  a panic an unresponsive shell a wrong exit code or a per crash leak
#   SKIP  nothing could run

[CmdletBinding()]
param(
    [string]$Image,
    [string]$OutDir,
    # Which fault to repeat Each maps to an exit code the kernel documents in
    # kernel/src/interrupts.rs
    [ValidateSet("bad_ud2", "bad_divzero", "bad_kernel", "bad_unmapped", "bad_privileged")]
    [string]$Program = "bad_ud2",
    [int]$Crashes = 20,
    [int]$MonitorPort = 45890,
    [int]$BootTimeout = 120,
    [switch]$AllowDirty
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot

# From kernel/src/interrupts.rs
#   EXIT_CODE_SEGV = 139  page fault from CPL=3
#   EXIT_CODE_ILL  = 132  general protection fault or invalid opcode
#   EXIT_CODE_FPE  = 136  divide error
$ExpectedExit = @{
    bad_ud2        = 132
    bad_divzero    = 136
    bad_kernel     = 139
    bad_unmapped   = 139
    bad_privileged = 132
}

$Results = [System.Collections.Generic.List[object]]::new()
$Notes = [System.Collections.Generic.List[string]]::new()

function Add-Result {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][ValidateSet("PASS", "FAIL", "SKIP")][string]$Status,
        [string]$Evidence = ""
    )
    $Results.Add([pscustomobject]@{ name = $Name; status = $Status; evidence = $Evidence })
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
    $OutDir = Join-Path $ProjectRoot "target\fault-relaunch\$($Commit.Substring(0, 12))-$Timestamp"
}
$OutDir = [System.IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

Write-Host "=== TuwaiqOS crash and relaunch ===" -ForegroundColor Cyan
Write-Host "commit  : $Commit"
Write-Host "program : $Program crashed $Crashes times, expecting exit $($ExpectedExit[$Program])"
Write-Host "output  : $OutDir"
Write-Host ""

$QemuCommand = Get-Command qemu-system-x86_64 -ErrorAction SilentlyContinue
$Qemu = if ($QemuCommand) { $QemuCommand.Source } else { "C:\Program Files\qemu\qemu-system-x86_64.exe" }
$QemuPresent = Test-Path -LiteralPath $Qemu -PathType Leaf
$ImagePresent = Test-Path -LiteralPath $Image -PathType Leaf
if (-not $ImagePresent) { $Notes.Add("disk image not found at '$Image'") }
if (-not $QemuPresent) { $Notes.Add("qemu-system-x86_64 not found in PATH") }

$KeyMap = @{ ' ' = 'spc'; '_' = 'shift-minus'; '/' = 'slash'; '.' = 'dot'; '-' = 'minus' }

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

function Read-BumpCursor {
    param([string]$Text)
    $hits = [regex]::Matches($Text, 'Frame bump cursor before:\s*(\d+)')
    if ($hits.Count -eq 0) { return $null }
    [int]$hits[$hits.Count - 1].Groups[1].Value
}

if (-not $ImagePresent -or -not $QemuPresent) {
    Add-Result -Name "fault-relaunch" -Status "SKIP" -Evidence ($Notes -join "; ")
}
else {
    $disk = Join-Path $OutDir "disk.img"
    Copy-Item -LiteralPath $Image -Destination $disk
    $serial = Join-Path $OutDir "serial.log"

    $qemuArgs = @(
        "-drive", "format=raw,file=$disk",
        "-m", "128M",
        "-display", "none",
        "-no-reboot",
        "-serial", "file:$serial",
        "-monitor", "telnet:127.0.0.1:$MonitorPort,server,nowait"
    )
    $proc = Start-Process -FilePath $Qemu -ArgumentList $qemuArgs -PassThru
    $client = $null
    $crashed = 0
    $wrongExit = 0
    $unresponsive = 0
    $bumpAfterWarmup = $null
    $bumpFinal = $null

    try {
        $deadline = (Get-Date).AddSeconds($BootTimeout)
        $reached = $false
        while ((Get-Date) -lt $deadline) {
            Start-Sleep -Seconds 3
            if (Test-Path -LiteralPath $serial) {
                $text = Get-Content -LiteralPath $serial -Raw -ErrorAction SilentlyContinue
                if ($text -and $text -match 'beat #(?:[3-9]|\d{2,})\b') { $reached = $true; break }
            }
            if ($proc.HasExited) { break }
        }

        if (-not $reached) {
            Add-Result -Name "fault-relaunch" -Status "FAIL" -Evidence "kernel never reached a shell within ${BootTimeout}s"
        }
        else {
            $tcp = [System.Net.Sockets.TcpClient]::new("127.0.0.1", $MonitorPort)
            $client = $tcp
            $stream = $tcp.GetStream()
            Start-Sleep -Seconds 1

            $expected = $ExpectedExit[$Program]

            for ($i = 1; $i -le $Crashes; $i++) {
                $before = (Get-Content -LiteralPath $serial -Raw).Length

                Send-Line -Stream $stream -Line "runelf $Program" -Settle 4.0

                # `uptime` is the liveness question. A shell that answers it
                # after a crash has survived the crash, which is the whole
                # claim being tested. It is asked every time rather than once
                # at the end so a failure is pinned to a crash number.
                Send-Line -Stream $stream -Line "uptime" -Settle 2.5

                $slice = (Get-Content -LiteralPath $serial -Raw).Substring($before)

                if ($slice -match "exit_code=(\d+)") {
                    $crashed++
                    if ([int]$Matches[1] -ne $expected) {
                        $wrongExit++
                        $Notes.Add("crash $i exited $($Matches[1]) not $expected")
                    }
                }
                else {
                    $Notes.Add("crash $i produced no exit code")
                }

                if ($slice -notmatch "Uptime: \d+ s") {
                    $unresponsive++
                    $Notes.Add("shell did not answer after crash $i")
                }

                if ($i % 5 -eq 0) { Write-Host "  $i of $Crashes" -ForegroundColor DarkGray }

                # The cursor is sampled after the first crash rather than
                # before it, for the same reason the memory stress suite does
                # so: a freshly booted system has never had to satisfy this
                # peak and is entitled to bump once.
                if ($i -eq 1 -or $i -eq $Crashes) {
                    Send-Line -Stream $stream -Line "reap 3" -Settle 6.0
                    Send-Line -Stream $stream -Line "spawnfail 1" -Settle 4.0
                    $cursor = Read-BumpCursor (Get-Content -LiteralPath $serial -Raw)
                    if ($i -eq 1) { $bumpAfterWarmup = $cursor } else { $bumpFinal = $cursor }
                }
            }
        }
    }
    finally {
        if ($client) { $client.Close() }
        if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    }

    if ($crashed -gt 0) {
        $text = Get-Content -LiteralPath $serial -Raw
        $panicked = [bool]($text -match 'KERNEL PANIC|EXCEPTION: DOUBLE FAULT')
        $trapped = ([regex]::Matches($text, 'trapped safely from CPL=3')).Count

        if ($panicked) {
            Add-Result -Name "kernel survives every crash" -Status "FAIL" -Evidence "the kernel panicked"
        }
        else {
            Add-Result -Name "kernel survives every crash" -Status "PASS" `
                -Evidence "$crashed crashes and $trapped CPL=3 faults trapped with no panic"
        }

        if ($crashed -lt $Crashes) {
            Add-Result -Name "every relaunch ran" -Status "FAIL" `
                -Evidence "only $crashed of $Crashes produced an exit code"
        }
        else {
            Add-Result -Name "every relaunch ran" -Status "PASS" -Evidence "$Crashes of $Crashes"
        }

        if ($wrongExit -gt 0) {
            Add-Result -Name "exit code stays consistent" -Status "FAIL" `
                -Evidence "$wrongExit of $crashed did not exit $($ExpectedExit[$Program])"
        }
        else {
            Add-Result -Name "exit code stays consistent" -Status "PASS" `
                -Evidence "all $crashed exited $($ExpectedExit[$Program])"
        }

        if ($unresponsive -gt 0) {
            Add-Result -Name "shell answers after every crash" -Status "FAIL" `
                -Evidence "no answer after $unresponsive of $crashed"
        }
        else {
            Add-Result -Name "shell answers after every crash" -Status "PASS" `
                -Evidence "answered all $crashed times"
        }

        if ($null -ne $bumpAfterWarmup -and $null -ne $bumpFinal) {
            $delta = $bumpFinal - $bumpAfterWarmup
            if ($delta -eq 0) {
                Add-Result -Name "no frames leaked per crash" -Status "PASS" `
                    -Evidence "cursor stayed at $bumpFinal across $($Crashes - 1) further crashes"
            }
            else {
                $per = [math]::Round($delta / [math]::Max(1, $Crashes - 1), 2)
                Add-Result -Name "no frames leaked per crash" -Status "FAIL" `
                    -Evidence "cursor went $bumpAfterWarmup to $bumpFinal which is about $per frames per crash"
            }
        }
        else {
            Add-Result -Name "no frames leaked per crash" -Status "SKIP" -Evidence "the cursor could not be read"
        }
    }
    elseif ($Results.Count -eq 0) {
        Add-Result -Name "fault-relaunch" -Status "FAIL" -Evidence "no crash produced a result"
    }
}

$pass = @($Results | Where-Object { $_.status -eq "PASS" }).Count
$fail = @($Results | Where-Object { $_.status -eq "FAIL" }).Count
$skip = @($Results | Where-Object { $_.status -eq "SKIP" }).Count
$verdict = if ($fail -gt 0) { "FAIL" } elseif ($pass -eq 0) { "SKIP" } elseif ($skip -gt 0) { "PARTIAL" } else { "PASS" }

[pscustomobject]@{
    verdict = $verdict
    commit  = $Commit
    program = $Program
    crashes = $Crashes
    counts  = [pscustomobject]@{ pass = $pass; fail = $fail; skip = $skip }
    notes   = @($Notes)
    results = @($Results)
} | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $OutDir "results.json") -Encoding utf8

[pscustomobject]@{
    suite     = "fault-relaunch"
    commit    = $Commit
    generated = (Get-Date).ToUniversalTime().ToString("o")
    verdict   = $verdict
    results   = "results.json"
} | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $OutDir "manifest.json") -Encoding utf8

Write-Host ""
Write-Host ("verdict: {0}   pass {1}  fail {2}  skip {3}" -f $verdict, $pass, $fail, $skip) `
    -ForegroundColor $(switch ($verdict) { "PASS" { "Green" } "FAIL" { "Red" } default { "Yellow" } })

exit $(switch ($verdict) { "PASS" { 0 } "FAIL" { 1 } "PARTIAL" { 2 } default { 3 } })
