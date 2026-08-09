# Focused Phase 7 PCI, VirtIO, IPv4, DHCP, DNS, and teardown smoke test.

[CmdletBinding()]
param(
    [string]$Image,
    [string]$OutDir,
    [int]$MonitorPort = 46971,
    [int]$SerialPort = 46972,
    [switch]$AllowDirty
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot
if (-not $Image) { $Image = Join-Path $ProjectRoot "target\debug\boot-bios-tuwaiqos.img" }
$Image = [IO.Path]::GetFullPath($Image)
if (-not (Test-Path -LiteralPath $Image -PathType Leaf)) { throw "Image not found: $Image" }
$Dirty = @(& git status --porcelain=v1 --untracked-files=all)
if ($Dirty.Count -gt 0 -and -not $AllowDirty) {
    throw "Worktree is dirty; commit the candidate or pass -AllowDirty for development."
}
$Commit = (& git rev-parse HEAD).Trim()
if (-not $OutDir) {
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $OutDir = Join-Path $ProjectRoot "target\phase7-smoke\$($Commit.Substring(0, 12))-$stamp"
}
$OutDir = [IO.Path]::GetFullPath($OutDir)
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$TestImage = Join-Path $OutDir "phase7-os.img"
$BlockImage = Join-Path $OutDir "phase7-virtio-block.raw"
$SerialLog = Join-Path $OutDir "serial.log"
$ResultsJson = Join-Path $OutDir "results.json"
$ManifestJson = Join-Path $OutDir "manifest.json"
Copy-Item -LiteralPath $Image -Destination $TestImage
$block = [IO.File]::Open($BlockImage, 'Create', 'ReadWrite', 'Read')
try {
    $block.SetLength(1MB)
    $marker = [Text.Encoding]::ASCII.GetBytes("TUWAIQ-PHASE7-VIRTIO-BLOCK")
    $block.Write($marker, 0, $marker.Length)
} finally { $block.Dispose() }

$QemuCommand = Get-Command qemu-system-x86_64 -ErrorAction SilentlyContinue
$Qemu = if ($QemuCommand) { $QemuCommand.Source } else { "C:\Program Files\qemu\qemu-system-x86_64.exe" }
if (-not (Test-Path -LiteralPath $Qemu -PathType Leaf)) { throw "qemu-system-x86_64 not found." }
$SerialText = [Text.StringBuilder]::new()
$Results = [Collections.Generic.List[object]]::new()
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
        [void]$SerialText.Append([Text.Encoding]::ASCII.GetString($buffer, 0, $count))
    }
}

function Save-Evidence {
    Pump-Serial
    [IO.File]::WriteAllText($SerialLog, $SerialText.ToString(), [Text.Encoding]::UTF8)
    $Results | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $ResultsJson -Encoding UTF8
    [pscustomobject]@{
        schema = 1
        git_commit = $Commit
        source_image_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $Image).Hash.ToLowerInvariant()
        qemu = $Qemu
        machine = "pc"
        block_device = "virtio-blk-pci legacy"
        network_device = "virtio-net-pci legacy"
        network_backend = "QEMU user network"
        verdict = if ($Results.Count -eq 8) { "PASS" } else { "INCOMPLETE" }
    } | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath $ManifestJson -Encoding UTF8
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
        $client = [Net.Sockets.TcpClient]::new()
        try { $client.Connect("127.0.0.1", $Port); return $client } catch {
            $client.Dispose()
            Start-Sleep -Milliseconds 200
        }
    }
    throw "Timed out connecting to $Name on port $Port."
}

function Invoke-Monitor {
    param([string]$Command)
    $MonitorWriter.WriteLine($Command)
    $MonitorWriter.Flush()
    Start-Sleep -Milliseconds 35
}

function Send-Text {
    param([string]$Text)
    $map = @{
        ' ' = 'spc'; '-' = 'minus'; '_' = 'shift-minus'; '/' = 'slash'; '.' = 'dot'; ':' = 'shift-semicolon'
    }
    foreach ($character in $Text.ToCharArray()) {
        $value = [string]$character
        if ($map.ContainsKey($value)) { Invoke-Monitor "sendkey $($map[$value])" }
        elseif ($value -cmatch '^[a-z0-9]$') { Invoke-Monitor "sendkey $value" }
        else { throw "No sendkey mapping for '$value'." }
    }
}

function Wait-Regex {
    param([string]$Pattern, [int]$StartOffset, [int]$TimeoutSeconds)
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        Pump-Serial 100
        Test-Health
        $all = $SerialText.ToString()
        if ($all.Length -ge $StartOffset -and $all.Substring($StartOffset) -match $Pattern) {
            Save-Evidence
            return $Matches[0]
        }
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
        "-drive", "if=none,id=osdisk,format=raw,file=$TestImage",
        "-device", "ide-hd,drive=osdisk,bus=ide.0,unit=0,bootindex=1",
        "-drive", "if=none,id=p7blk,format=raw,file=$BlockImage",
        "-device", "virtio-blk-pci,drive=p7blk,disable-modern=on,bootindex=2",
        "-netdev", "user,id=p7net,dns=10.0.2.3",
        "-device", "virtio-net-pci,netdev=p7net,disable-modern=on,mac=52:54:00:70:07:01",
        "-m", "128M", "-display", "none",
        "-serial", "tcp:127.0.0.1:$SerialPort,server,nowait",
        "-monitor", "tcp:127.0.0.1:$MonitorPort,server,nowait", "-no-shutdown"
    )
    $Proc = Start-Process -FilePath $Qemu -ArgumentList $arguments -PassThru -WindowStyle Hidden
    $SerialClient = Connect-Tcp $SerialPort "serial"
    $MonitorClient = Connect-Tcp $MonitorPort "monitor"
    $SerialStream = $SerialClient.GetStream()
    $MonitorStream = $MonitorClient.GetStream()
    $MonitorWriter = [IO.StreamWriter]::new($MonitorStream)
    $MonitorWriter.AutoFlush = $true

    [void](Wait-Regex 'task heartbeat: beat #1(?:\r?\n|$)' 0 120)
    [void](Wait-Regex 'hal: PCI discovery complete \([0-9]+ functions, truncated=false\)' 0 5)
    Add-Pass "PCI discovery" "bounded inventory completed without truncation"
    [void](Wait-Regex 'virtio-blk: PASS capacity=2048 sectors sector0-checksum=1874 teardown=reset\+scrub' 0 5)
    Add-Pass "VirtIO block" "sector 0 DMA read matched deterministic marker and teardown contract"
    [void](Wait-Regex 'virtio-net: bind 00:[0-9a-f]{2}\.[0-7] MAC=52:54:00:70:07:01 polling DMA=[0-9]+ bytes' 0 5)
    [void](Wait-Regex 'net: VirtIO Ethernet link=up IPv4=unconfigured DHCP=ready DNS=ready' 0 5)
    Add-Pass "VirtIO network link" "stable MAC, link up, polling ownership, fixed DMA"

    $segment = Invoke-Command "net dhcp" @('DHCP: configured 10\.0\.2\.15/24') 30
    Add-Pass "DHCP and IPv4" ([regex]::Match($segment, 'DHCP: configured[^\r\n]*').Value)
    $segment = Invoke-Command "net dns example.com" @('DNS: example\.com -> [0-9]+\.[0-9]+\.[0-9]+\.[0-9]+') 20
    Add-Pass "DNS traffic" ([regex]::Match($segment, 'DNS: example\.com[^\r\n]*').Value)
    $segment = Invoke-Command "net status" @('VirtIO Ethernet: link up', 'IPv4: 10\.0\.2\.15/24', 'Gateway: 10\.0\.2\.2', 'DNS: 10\.0\.2\.3') 10
    Add-Pass "bounded network state" "status retained DHCP route and DNS configuration"
    $first = Invoke-Command "runelf bad_net" @('bad_net: PASS', 'exit_code=0') 30
    $second = Invoke-Command "runelf bad_net" @('bad_net: PASS', 'exit_code=0') 30
    if ($first -notmatch 'exit-owned socket acquired for cleanup proof' -or $second -notmatch 'exit-owned socket acquired for cleanup proof') {
        throw "UDP process-exit cleanup proof was incomplete."
    }
    Add-Pass "hostile UDP ABI" "invalid pointers/ranges/stale handles rejected; process-exit socket ownership reclaimed"
    $segment = Invoke-Command "net shutdown-test" @('net: PASS failure-cleanup device-reset DMA-scrub ownership-released') 10
    Add-Pass "failure cleanup" "device reset, DMA scrub, and ownership release completed"
    Pump-Serial 500
    Test-Health
    Save-Evidence
    $MonitorWriter.WriteLine("quit")
    Start-Sleep -Milliseconds 300
    Write-Host "Phase 7 focused smoke PASS: $OutDir" -ForegroundColor Green
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
