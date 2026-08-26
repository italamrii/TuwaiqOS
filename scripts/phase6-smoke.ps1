# Focused Phase 6 VFS and Tuwaiq AI Preview smoke test.

[CmdletBinding()]
param(
    [string]$Image,
    [string]$OutDir,
    [int]$MonitorPort = 45861,
    [int]$SerialPort = 45862,
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
    $OutDir = Join-Path $ProjectRoot "target\phase6-smoke\$($Commit.Substring(0, 12))-$Timestamp"
}
$OutDir = [System.IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$TestImage = Join-Path $OutDir "smoke.img"
Copy-Item -LiteralPath $Image -Destination $TestImage

$QemuCommand = Get-Command qemu-system-x86_64 -ErrorAction SilentlyContinue
$Qemu = if ($QemuCommand) { $QemuCommand.Source } else { "C:\Program Files\qemu\qemu-system-x86_64.exe" }
if (-not (Test-Path -LiteralPath $Qemu -PathType Leaf)) { throw "qemu-system-x86_64 not found." }

# -WindowStyle is a Windows-only Start-Process parameter; Unix PowerShell
# rejects it outright. QEMU is already headless (-display none) -- the style
# only hides the extra console window Windows would open for the child.
$HiddenWindowStyle = if ($PSVersionTable.PSVersion.Major -lt 6 -or $IsWindows) {
    @{ WindowStyle = 'Hidden' }
} else {
    @{}
}

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
    if ($all -match 'KERNEL PANIC|kernel page fault|EXCEPTION: DOUBLE FAULT') {
        throw "Kernel panic/fault detected."
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

function Invoke-Command {
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
    $Proc = Start-Process -FilePath $Qemu -ArgumentList $arguments -PassThru @HiddenWindowStyle
    $SerialClient = Connect-Tcp $SerialPort "serial"
    $MonitorClient = Connect-Tcp $MonitorPort "monitor"
    $SerialStream = $SerialClient.GetStream()
    $MonitorStream = $MonitorClient.GetStream()
    $MonitorWriter = [System.IO.StreamWriter]::new($MonitorStream)
    $MonitorWriter.AutoFlush = $true

    [void](Wait-Regex 'task heartbeat: beat #1(?:\r?\n|$)' 0 90)
    [void](Wait-Regex 'vfs: mounted TuwaiqFS v3 at /' 0 5)
    Add-Pass "boot and root mount" "kernel reached scheduler with TuwaiqFS mounted at /"

    $segment = Invoke-Command "vfstest" @('vfs: PASS', 'file-api: PASS') 120
    Add-Pass "VFS and hostile file ABI" ([regex]::Match($segment, 'vfs: PASS[^\r\n]*').Value)

    $segment = Invoke-Command "aipreviewtest" @('ai-preview: PASS', 'inference unavailable', 'usermode: invalid opcode trapped safely from CPL=3') 120
    Add-Pass "AI service lifecycle and crash isolation" ([regex]::Match($segment, 'ai-preview: PASS[^\r\n]*').Value)

    [void](Invoke-Command "installapp hello" @('/apps/hello') 60)
    $segment = Invoke-Command "runfs /apps/hello" @('hello: Ring 3 ELF process alive', 'exit_code=0') 60
    Add-Pass "filesystem-backed ELF" "installed bytes were loaded from /apps/hello and exited 0"

    [void](Invoke-Command "cd /phase6-test")
    [void](Invoke-Command "pwd" @('/phase6-test'))
    [void](Invoke-Command "cat ./data.txt" @('phase6-data'))
    [void](Invoke-Command "cd ..")
    Add-Pass "shell paths and cd" "absolute, relative, dot, and parent paths resolved"

    Pump-Serial
    $desktopOffset = $SerialText.Length
    Send-Text "desktopaitest"
    Invoke-Monitor "sendkey ret"
    [void](Wait-Regex 'desktop-ai-preview: window visible service_exit=Some\(0\)' $desktopOffset 90)
    $screenshot = Join-Path $OutDir "tuwaiq-ai-preview.ppm"
    Invoke-Monitor "screendump $($screenshot.Replace('\', '/'))" 300
    [void](Wait-Regex 'shell: command complete: desktopaitest(?:\r?\n|$)' $desktopOffset 90)
    Pump-Serial 100
    $desktopSegment = $SerialText.ToString().Substring($desktopOffset)
    if ($desktopSegment -notmatch 'desktop AI preview: PASS') { throw "Desktop AI preview test failed." }
    if (-not (Test-Path -LiteralPath $screenshot -PathType Leaf)) { throw "Preview screenshot missing." }
    Add-Pass "desktop AI preview surface" "launcher spawned Ring-3 service and visible honest-status window"

    Pump-Serial
    $rebootOffset = $SerialText.Length
    Send-Text "reboot"
    Invoke-Monitor "sendkey ret"
    [void](Wait-Regex 'TuwaiqOS v0\.5 kernel_main: booting' $rebootOffset 60)
    [void](Wait-Regex 'task heartbeat: beat #1(?:\r?\n|$)' $rebootOffset 90)
    $segment = Invoke-Command "runfs /apps/tuwaiq-ai" @('inference unavailable', 'exit_code=0') 60
    Add-Pass "binary persistence after genuine reboot" "filesystem AI ELF relaunched from the rebooted disk"

    Pump-Serial
    Test-Health
    Add-Pass "kernel health" "no panic, kernel page fault, double fault, or unexpected QEMU exit"
    Save-Evidence
    $MonitorWriter.WriteLine("quit")
    Start-Sleep -Milliseconds 300
    Write-Host "Phase 6 focused smoke PASS: $OutDir" -ForegroundColor Green
} finally {
    try { Save-Evidence } catch {}
    if ($null -ne $MonitorWriter) { try { $MonitorWriter.WriteLine("quit") } catch {} }
    foreach ($resource in @($MonitorWriter, $MonitorStream, $SerialStream, $MonitorClient, $SerialClient)) {
        if ($null -ne $resource) { try { $resource.Dispose() } catch {} }
    }
    if ($null -ne $Proc -and -not $Proc.HasExited) { Stop-Process -Id $Proc.Id -Force -ErrorAction SilentlyContinue }
}
