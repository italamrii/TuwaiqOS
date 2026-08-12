# Mixed-workload memory stress
#
# The existing suites measure one activity at a time and each from a clean
# start `spawnfail` covers failed loads `reap` covers normal exits
# `mmap_exhaustion` covers anonymous memory and `isolate` covers faults
#
# This runs them interleaved and repeatedly which is the case none of them
# reaches a leak that only appears on the fault path or only when one activity
# follows another
#
# The metric is the frame bump cursor and not the allocation count
#
# `allocated` counts every call to `allocate_frame` including the ones served
# from the free list so it climbs on a system that is recycling perfectly
# `kernel/src/paging.rs` says exactly that at `BootInfoFrameAllocator`
# The bump cursor only moves when a frame is taken that was never taken before
# so it is the one number that answers did anything leak
#
# Verdicts
#   PASS  the cursor stops moving and stays stopped
#   FAIL  the cursor grows on every round which is a leak per round
#   SKIP  nothing could run

[CmdletBinding()]
param(
    [string]$Image,
    [string]$OutDir,
    [int]$Rounds = 5,
    [int]$MonitorPort = 45880,
    [int]$BootTimeout = 120,
    [switch]$AllowDirty
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot

# One round
#
# Deliberately mixed The fault path the normal exit path anonymous memory and
# a rejected pointer each already have their own test in isolation What has
# never been measured is what happens when they follow one another
$Workload = @(
    "isolate bad_ud2"
    "isolate bad_kernel"
    "runelf hello"
    "runelf bad_mmap"
    "runelf bad_pointer"
    "isolate bad_divzero"
)

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
    $OutDir = Join-Path $ProjectRoot "target\memory-stress\$($Commit.Substring(0, 12))-$Timestamp"
}
$OutDir = [System.IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

Write-Host "=== TuwaiqOS memory stress ===" -ForegroundColor Cyan
Write-Host "commit : $Commit"
Write-Host "rounds : $Rounds of $($Workload.Count) programs"
Write-Host "output : $OutDir"
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

# `spawnfail` prints the cursor as part of its own report so it doubles as the
# only way to read it from the shell
function Read-BumpCursor {
    param([string]$Text)
    $hits = [regex]::Matches($Text, 'Frame bump cursor before:\s*(\d+)')
    if ($hits.Count -eq 0) { return $null }
    [int]$hits[$hits.Count - 1].Groups[1].Value
}

function Read-HeapUsed {
    param([string]$Text)
    $hits = [regex]::Matches($Text, 'Kernel heap: \d+ bytes \((\d+) used')
    if ($hits.Count -eq 0) { return $null }
    [int]$hits[$hits.Count - 1].Groups[1].Value
}

if (-not $ImagePresent -or -not $QemuPresent) {
    Add-Result -Name "memory-stress" -Status "SKIP" -Evidence ($Notes -join "; ")
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
    $samples = [System.Collections.Generic.List[object]]::new()

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
            Add-Result -Name "memory-stress" -Status "FAIL" -Evidence "kernel never reached a shell within ${BootTimeout}s"
        }
        else {
            $tcp = [System.Net.Sockets.TcpClient]::new("127.0.0.1", $MonitorPort)
            $client = $tcp
            $stream = $tcp.GetStream()
            Start-Sleep -Seconds 1

            # Let anything the shell started at boot finish before the baseline
            # or the first round absorbs it and looks like growth
            Send-Line -Stream $stream -Line "reap 3" -Settle 6.0
            Send-Line -Stream $stream -Line "sysinfo" -Settle 3.0
            Send-Line -Stream $stream -Line "spawnfail 1" -Settle 4.0
            $text = Get-Content -LiteralPath $serial -Raw
            $samples.Add([pscustomobject]@{
                    label = "baseline"
                    bump  = Read-BumpCursor $text
                    heap  = Read-HeapUsed $text
                })

            for ($round = 1; $round -le $Rounds; $round++) {
                Write-Host "  round $round of $Rounds" -ForegroundColor DarkGray
                foreach ($command in $Workload) {
                    Send-Line -Stream $stream -Line $command -Settle 3.0
                }
                Send-Line -Stream $stream -Line "reap 3" -Settle 6.0
                Send-Line -Stream $stream -Line "sysinfo" -Settle 3.0
                Send-Line -Stream $stream -Line "spawnfail 1" -Settle 4.0
                $text = Get-Content -LiteralPath $serial -Raw
                $samples.Add([pscustomobject]@{
                        label = "round $round"
                        bump  = Read-BumpCursor $text
                        heap  = Read-HeapUsed $text
                    })
            }
        }
    }
    finally {
        if ($client) { $client.Close() }
        if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    }

    if ($samples.Count -gt 1) {
        $text = Get-Content -LiteralPath $serial -Raw
        $panicked = [bool]($text -match 'KERNEL PANIC|EXCEPTION: DOUBLE FAULT')
        $trapped = ([regex]::Matches($text, 'trapped safely from CPL=3')).Count

        Write-Host ""
        Write-Host ("{0,-12} {1,12} {2,12}" -f "point", "bump cursor", "heap used")
        foreach ($s in $samples) {
            Write-Host ("{0,-12} {1,12} {2,12}" -f $s.label, $s.bump, $s.heap)
        }
        Write-Host ""

        if ($panicked) {
            Add-Result -Name "no panic under mixed load" -Status "FAIL" -Evidence "the kernel panicked"
        }
        else {
            Add-Result -Name "no panic under mixed load" -Status "PASS" `
                -Evidence "$trapped CPL=3 faults trapped and contained across $Rounds rounds"
        }

        # The first round is allowed to move the cursor
        # A freshly booted system has never had to satisfy this peak so it
        # bumps fresh frames once to build the pool What matters is whether it
        # keeps doing that
        $settled = @($samples | Select-Object -Skip 2)
        if ($settled.Count -lt 1 -or ($null -eq $settled[0].bump)) {
            Add-Result -Name "frame bump cursor settles" -Status "SKIP" `
                -Evidence "not enough rounds to tell growth from warm-up; use -Rounds 3 or more"
        }
        else {
            $first = $settled[0].bump
            $last = $settled[$settled.Count - 1].bump
            if ($last -eq $first) {
                Add-Result -Name "frame bump cursor settles" -Status "PASS" `
                    -Evidence "cursor stopped at $first and stayed there for $($settled.Count) rounds"
            }
            else {
                $perRound = [math]::Round(($last - $first) / [math]::Max(1, $settled.Count - 1), 1)
                Add-Result -Name "frame bump cursor settles" -Status "FAIL" `
                    -Evidence "cursor went $first to $last after warm-up which is about $perRound frames leaked per round"
            }
        }

        $heapFirst = $settled | Select-Object -First 1
        $heapLast = $settled | Select-Object -Last 1
        if ($null -ne $heapFirst.heap -and $null -ne $heapLast.heap) {
            $delta = $heapLast.heap - $heapFirst.heap
            if ($delta -eq 0) {
                Add-Result -Name "kernel heap settles" -Status "PASS" -Evidence "unchanged after warm-up"
            }
            else {
                Add-Result -Name "kernel heap settles" -Status "FAIL" -Evidence "$delta bytes after warm-up"
            }
        }
    }
    elseif ($Results.Count -eq 0) {
        Add-Result -Name "memory-stress" -Status "FAIL" -Evidence "no measurements were taken"
    }
}

$pass = @($Results | Where-Object { $_.status -eq "PASS" }).Count
$fail = @($Results | Where-Object { $_.status -eq "FAIL" }).Count
$skip = @($Results | Where-Object { $_.status -eq "SKIP" }).Count
$verdict = if ($fail -gt 0) { "FAIL" } elseif ($pass -eq 0) { "SKIP" } elseif ($skip -gt 0) { "PARTIAL" } else { "PASS" }

[pscustomobject]@{
    verdict = $verdict
    commit  = $Commit
    rounds  = $Rounds
    counts  = [pscustomobject]@{ pass = $pass; fail = $fail; skip = $skip }
    notes   = @($Notes)
    results = @($Results)
} | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $OutDir "results.json") -Encoding utf8

[pscustomobject]@{
    suite     = "memory-stress"
    commit    = $Commit
    generated = (Get-Date).ToUniversalTime().ToString("o")
    verdict   = $verdict
    results   = "results.json"
} | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $OutDir "manifest.json") -Encoding utf8

Write-Host ""
Write-Host ("verdict: {0}   pass {1}  fail {2}  skip {3}" -f $verdict, $pass, $fail, $skip) `
    -ForegroundColor $(switch ($verdict) { "PASS" { "Green" } "FAIL" { "Red" } default { "Yellow" } })

exit $(switch ($verdict) { "PASS" { 0 } "FAIL" { 1 } "PARTIAL" { 2 } default { 3 } })
