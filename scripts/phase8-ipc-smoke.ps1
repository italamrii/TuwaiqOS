# Focused Phase 8 IPC, capability, lifecycle, and essential regression suite.

[CmdletBinding()]
param(
    [string]$Image,
    [string]$OutDir,
    [int]$MonitorPort = 46881,
    [int]$SerialPort = 46882,
    [switch]$AllowDirty
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot
if (-not $Image) { $Image = Join-Path $ProjectRoot "target\debug\boot-bios-tuwaiqos.img" }
$Image = [System.IO.Path]::GetFullPath($Image)
if (-not (Test-Path -LiteralPath $Image -PathType Leaf)) {
    throw "Disk image not found at '$Image'. Run scripts\build.ps1 first."
}

$Dirty = @(& git status --porcelain=v1 --untracked-files=all)
if ($Dirty.Count -gt 0 -and -not $AllowDirty) {
    throw "Worktree is dirty; commit the candidate or pass -AllowDirty for development."
}
$Commit = (& git rev-parse HEAD).Trim()
$Timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
if (-not $OutDir) {
    $OutDir = Join-Path $ProjectRoot "target\phase8-ipc-smoke\$($Commit.Substring(0, 12))-$Timestamp"
}
$OutDir = [System.IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$TestImage = Join-Path $OutDir "phase8.img"
Copy-Item -LiteralPath $Image -Destination $TestImage

$QemuCommand = Get-Command qemu-system-x86_64 -ErrorAction SilentlyContinue
$Qemu = if ($QemuCommand) { $QemuCommand.Source } else { "C:\Program Files\qemu\qemu-system-x86_64.exe" }
if (-not (Test-Path -LiteralPath $Qemu -PathType Leaf)) { throw "qemu-system-x86_64 not found." }

$SerialLog = Join-Path $OutDir "serial.log"
$ResultsJson = Join-Path $OutDir "results.json"
$SerialText = [System.Text.StringBuilder]::new()
$Results = [System.Collections.Generic.List[object]]::new()
$Proc = $null
$MonitorClient = $null
$SerialClient = $null
$MonitorStream = $null
$SerialStream = $null
$MonitorWriter = $null

function Pump-Serial {
    param([int]$WaitMilliseconds = 0)
    if ($WaitMilliseconds -gt 0) { Start-Sleep -Milliseconds $WaitMilliseconds }
    while ($null -ne $SerialStream -and $SerialStream.DataAvailable) {
        $buffer = New-Object byte[] 16384
        $count = $SerialStream.Read($buffer, 0, $buffer.Length)
        if ($count -le 0) { break }
        [void]$SerialText.Append([System.Text.Encoding]::ASCII.GetString($buffer, 0, $count))
    }
}

function Save-Evidence {
    Pump-Serial
    [System.IO.File]::WriteAllText($SerialLog, $SerialText.ToString(), [System.Text.Encoding]::UTF8)
    $Results | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $ResultsJson -Encoding UTF8
}

function Test-Health {
    $all = $SerialText.ToString()
    if ($all -match 'KERNEL PANIC|kernel page fault|EXCEPTION: DOUBLE FAULT|deadlock') {
        throw "Kernel panic, fault, or deadlock marker detected."
    }
    if ($null -ne $Proc -and $Proc.HasExited) { throw "QEMU exited unexpectedly." }
}

function Connect-Tcp {
    param([int]$Port, [string]$Name)
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline) {
        $client = [System.Net.Sockets.TcpClient]::new()
        try { $client.Connect("127.0.0.1", $Port); return $client } catch {
            $client.Dispose(); Start-Sleep -Milliseconds 200
        }
    }
    throw "Timed out connecting to $Name on port $Port."
}

function Invoke-Monitor {
    param([string]$Command, [int]$SettleMilliseconds = 35)
    $MonitorWriter.WriteLine($Command)
    Start-Sleep -Milliseconds $SettleMilliseconds
    while ($MonitorStream.DataAvailable) {
        $discard = New-Object byte[] 4096
        [void]$MonitorStream.Read($discard, 0, $discard.Length)
    }
    Pump-Serial
    Test-Health
}

function Send-Text {
    param([string]$Text)
    $map = @{ ' ' = 'spc'; '-' = 'minus'; '.' = 'dot'; '/' = 'slash' }
    foreach ($character in $Text.ToCharArray()) {
        $value = [string]$character
        if ($map.ContainsKey($value)) { Invoke-Monitor "sendkey $($map[$value])" }
        elseif ($value -cmatch '^[a-z0-9]$') { Invoke-Monitor "sendkey $value" }
        else { throw "No sendkey mapping for '$value'." }
    }
}

function Wait-Regex {
    param([string]$Pattern, [int]$Offset = 0, [int]$TimeoutSeconds = 45)
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        Pump-Serial 100
        Test-Health
        $all = $SerialText.ToString()
        if ($Offset -gt $all.Length) { $Offset = $all.Length }
        if ($all.Substring($Offset) -match $Pattern) { return $Matches[0] }
    }
    Save-Evidence
    throw "Timed out waiting for '$Pattern'."
}

function Add-Pass {
    param([string]$Name, [string]$Evidence)
    $Results.Add([pscustomobject]@{ name = $Name; status = "PASS"; evidence = $Evidence })
    Write-Host "[PASS] $Name - $Evidence" -ForegroundColor Green
}

function Invoke-ShellCommand {
    param([string]$Command, [string[]]$Expected = @(), [int]$TimeoutSeconds = 60)
    Pump-Serial
    $offset = $SerialText.Length
    Send-Text $Command
    Invoke-Monitor "sendkey ret"
    [void](Wait-Regex "shell: command complete: $([regex]::Escape($Command))(?:\r?\n|$)" $offset $TimeoutSeconds)
    Pump-Serial 100
    $segment = $SerialText.ToString().Substring($offset)
    foreach ($pattern in $Expected) {
        if ($segment -notmatch $pattern) { throw "'$Command' missed expected '$pattern'." }
    }
    Test-Health
    return $segment
}

try {
    $arguments = @(
        "-drive", "format=raw,file=$TestImage", "-m", "128M", "-display", "none",
        "-serial", "tcp:127.0.0.1:$SerialPort,server,nowait",
        "-monitor", "tcp:127.0.0.1:$MonitorPort,server,nowait", "-no-shutdown"
    )
    $Proc = Start-Process -FilePath $Qemu -ArgumentList $arguments -PassThru -WindowStyle Hidden
    $SerialClient = Connect-Tcp $SerialPort "serial"
    $MonitorClient = Connect-Tcp $MonitorPort "monitor"
    $SerialStream = $SerialClient.GetStream()
    $MonitorStream = $MonitorClient.GetStream()
    $MonitorWriter = [System.IO.StreamWriter]::new($MonitorStream)
    $MonitorWriter.AutoFlush = $true

    [void](Wait-Regex 'task heartbeat: beat #1(?:\r?\n|$)' 0 90)
    [void](Wait-Regex 'vfs: mounted TuwaiqFS v3 at /' 0 5)
    Add-Pass "boot and graceful optional-device startup" "scheduler and writable VFS reached with headless display"

    $segment = Invoke-ShellCommand "ipctest 20" @(
        'ipc-provider: PASS',
        'ipc-client: PASS',
        'ipc-intruder: PASS',
        'ipc-crash-client: PASS',
        'ipc-timeout-client: PASS',
        'ipc-timeout-provider: PASS',
        'ipc-backpressure-client: PASS',
        'ipc-backpressure-provider: PASS',
        'bad-ipc: PASS',
        'ipc: PASS cycles=20',
        'endpoints=0->0 capabilities=0->0 calls=0->0 scopes=0->0 queued=0->0 waiters=0->0'
    ) 180
    Add-Pass "IPC, capabilities, hostile inputs, and lifecycle" ([regex]::Match($segment, 'ipc: PASS cycles=20[^\r\n]*').Value)

    $segment = Invoke-ShellCommand "vfstest" @('vfs: PASS', 'file-api: PASS') 120
    Add-Pass "Phase 6 VFS regression" ([regex]::Match($segment, 'vfs: PASS[^\r\n]*').Value)
    $segment = Invoke-ShellCommand "storagetest" @('storage: PASS') 120
    Add-Pass "persistent storage mutation regression" ([regex]::Match($segment, 'storage: PASS[^\r\n]*').Value)
    $segment = Invoke-ShellCommand "net status" @('Network') 30
    Add-Pass "merged-base network behavior" ([regex]::Match($segment, 'Network[^\r\n]*').Value)

    Pump-Serial
    $rebootOffset = $SerialText.Length
    Send-Text "reboot"
    Invoke-Monitor "sendkey ret"
    [void](Wait-Regex 'TuwaiqOS v0\.5 kernel_main: booting' $rebootOffset 60)
    [void](Wait-Regex 'task heartbeat: beat #1(?:\r?\n|$)' $rebootOffset 90)
    $segment = Invoke-ShellCommand "cat /data/ipc-provider/shared.txt" @('delegated-updated') 30
    Add-Pass "delegated file persistence after genuine reboot" "capability-written provider data survived reboot on the tested disk"
    $segment = Invoke-ShellCommand "ipctest 1" @('ipc: PASS cycles=1') 120
    Add-Pass "post-reboot IPC relaunch" ([regex]::Match($segment, 'ipc: PASS cycles=1[^\r\n]*').Value)

    Pump-Serial
    Test-Health
    Add-Pass "kernel health" "no panic, Ring-0 page fault, double fault, deadlock marker, or unexpected QEMU exit"
    Save-Evidence
    $MonitorWriter.WriteLine("quit")
    Start-Sleep -Milliseconds 300
    Write-Host "Phase 8 focused smoke PASS: $OutDir" -ForegroundColor Green
} finally {
    try { Save-Evidence } catch {}
    if ($null -ne $MonitorWriter) { try { $MonitorWriter.WriteLine("quit") } catch {} }
    foreach ($resource in @($MonitorWriter, $MonitorStream, $SerialStream, $MonitorClient, $SerialClient)) {
        if ($null -ne $resource) { try { $resource.Dispose() } catch {} }
    }
    if ($null -ne $Proc -and -not $Proc.HasExited) {
        Stop-Process -Id $Proc.Id -Force -ErrorAction SilentlyContinue
    }
}
