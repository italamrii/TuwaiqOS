# Focused Phase 7B NVMe, persistence, fallback, and cleanup acceptance.

[CmdletBinding()]
param(
    [string]$Image,
    [string]$OutDir,
    [int]$MonitorPort = 47971,
    [int]$SerialPort = 47972,
    [switch]$AllowDirty
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot
if (-not $Image) { $Image = Join-Path $ProjectRoot 'target\debug\boot-bios-tuwaiqos.img' }
$Image = [IO.Path]::GetFullPath($Image)
if (-not (Test-Path -LiteralPath $Image -PathType Leaf)) { throw "Image not found: $Image" }
$Dirty = @(& git status --porcelain=v1 --untracked-files=all)
if ($Dirty.Count -gt 0 -and -not $AllowDirty) { throw 'Worktree is dirty; commit or pass -AllowDirty.' }
$Commit = (& git rev-parse HEAD).Trim()
if (-not $OutDir) {
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $OutDir = Join-Path $ProjectRoot "target\phase7b-nvme-smoke\$($Commit.Substring(0, 12))-$stamp"
}
$OutDir = [IO.Path]::GetFullPath($OutDir)
[void](New-Item -ItemType Directory -Force -Path $OutDir)

$NvmeImage = Join-Path $OutDir 'nvme-os.img'
$AtaImage = Join-Path $OutDir 'ata-os.img'
$MalformedImage = Join-Path $OutDir 'malformed-ns.raw'
$VirtioImage = Join-Path $OutDir 'virtio-block.raw'
$SerialLog = Join-Path $OutDir 'serial.log'
$ResultsJson = Join-Path $OutDir 'results.json'
$ManifestJson = Join-Path $OutDir 'manifest.json'
Copy-Item -LiteralPath $Image -Destination $NvmeImage
Copy-Item -LiteralPath $Image -Destination $AtaImage

$stream = [IO.File]::Open($MalformedImage, 'Create', 'ReadWrite', 'Read')
try { $stream.SetLength(8MB) } finally { $stream.Dispose() }
$stream = [IO.File]::Open($VirtioImage, 'Create', 'ReadWrite', 'Read')
try {
    $stream.SetLength(1MB)
    $marker = [Text.Encoding]::ASCII.GetBytes('TUWAIQ-PHASE7-VIRTIO-BLOCK')
    $stream.Write($marker, 0, $marker.Length)
} finally { $stream.Dispose() }

$QemuCommand = Get-Command qemu-system-x86_64 -ErrorAction SilentlyContinue
$Qemu = if ($QemuCommand) { $QemuCommand.Source } else { 'C:\Program Files\qemu\qemu-system-x86_64.exe' }
if (-not (Test-Path -LiteralPath $Qemu -PathType Leaf)) { throw 'qemu-system-x86_64 not found.' }

$Results = [Collections.Generic.List[object]]::new()
$AllSerial = [Text.StringBuilder]::new()
$SerialText = $null
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
    [IO.File]::WriteAllText($SerialLog, $AllSerial.ToString() + $SerialText.ToString(), [Text.Encoding]::UTF8)
    $Results | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $ResultsJson -Encoding UTF8
    [pscustomobject]@{
        schema = 1
        git_commit = $Commit
        source_image_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $Image).Hash.ToLowerInvariant()
        qemu = $Qemu
        nvme_device = 'QEMU NVMe PCI, 512-byte boot namespace'
        malformed_namespace = 'QEMU NVMe PCI, 4096-byte unsupported namespace'
        fallback = 'ATA boot plus legacy VirtIO block probe'
        verdict = if ($Results.Count -eq 10) { 'PASS' } else { 'INCOMPLETE' }
    } | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath $ManifestJson -Encoding UTF8
}

function Test-Health {
    $all = $SerialText.ToString()
    if ($all -match 'KERNEL PANIC|EXCEPTION: DOUBLE FAULT|fault context:.*Ring 0') {
        throw 'Kernel panic/fault detected.'
    }
    if ($null -ne $Proc -and $Proc.HasExited) { throw 'QEMU exited unexpectedly.' }
}

function Connect-Tcp {
    param([int]$Port, [string]$Name)
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline) {
        $client = [Net.Sockets.TcpClient]::new()
        try { $client.Connect('127.0.0.1', $Port); return $client } catch {
            $client.Dispose()
            Start-Sleep -Milliseconds 200
        }
    }
    throw "Timed out connecting to $Name on port $Port."
}

function Start-TestVm {
    param([string]$Label, [string[]]$DeviceArguments)
    $script:SerialText = [Text.StringBuilder]::new()
    [void]$AllSerial.Append("`r`n===== $Label =====`r`n")
    $arguments = @($DeviceArguments) + @(
        '-m', '128M', '-display', 'none', '-serial', "tcp:127.0.0.1:$SerialPort,server,nowait",
        '-monitor', "tcp:127.0.0.1:$MonitorPort,server,nowait", '-no-shutdown'
    )
    $errorLog = Join-Path $OutDir (($Label -replace '[^A-Za-z0-9]+', '-').ToLowerInvariant() + '-qemu-error.log')
    $script:Proc = Start-Process -FilePath $Qemu -ArgumentList $arguments -PassThru -WindowStyle Hidden -RedirectStandardError $errorLog
    $script:SerialClient = Connect-Tcp $SerialPort 'serial'
    $script:MonitorClient = Connect-Tcp $MonitorPort 'monitor'
    $script:SerialStream = $SerialClient.GetStream()
    $script:MonitorStream = $MonitorClient.GetStream()
    $script:MonitorWriter = [IO.StreamWriter]::new($MonitorStream)
    $MonitorWriter.AutoFlush = $true
}

function Stop-TestVm {
    Pump-Serial 100
    if ($null -ne $SerialText) {
        [void]$AllSerial.Append($SerialText.ToString())
        $script:SerialText = [Text.StringBuilder]::new()
    }
    if ($null -ne $MonitorWriter) { try { $MonitorWriter.WriteLine('quit') } catch {} }
    foreach ($resource in @($MonitorWriter, $MonitorStream, $SerialStream, $MonitorClient, $SerialClient)) {
        if ($null -ne $resource) { try { $resource.Dispose() } catch {} }
    }
    if ($null -ne $Proc -and -not $Proc.HasExited) {
        Stop-Process -Id $Proc.Id -Force
        Wait-Process -Id $Proc.Id -Timeout 5 -ErrorAction SilentlyContinue
    }
    $script:Proc = $null
    $script:MonitorWriter = $null
    $script:MonitorStream = $null
    $script:SerialStream = $null
    $script:MonitorClient = $null
    $script:SerialClient = $null
    Start-Sleep -Milliseconds 300
}

function Invoke-Monitor {
    param([string]$Command)
    $MonitorWriter.WriteLine($Command)
    $MonitorWriter.Flush()
    Start-Sleep -Milliseconds 35
}

function Send-Text {
    param([string]$Text)
    $map = @{ ' ' = 'spc'; '-' = 'minus'; '_' = 'shift-minus'; '/' = 'slash'; '.' = 'dot'; ':' = 'shift-semicolon' }
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
        if ($all.Length -ge $StartOffset -and $all.Substring($StartOffset) -match $Pattern) { return $Matches[0] }
    }
    Save-Evidence
    throw "Timed out waiting for '$Pattern'."
}

function Invoke-ShellCommand {
    param([string]$Command, [string[]]$Expected = @(), [int]$TimeoutSeconds = 60)
    Pump-Serial
    $offset = $SerialText.Length
    Send-Text $Command
    Invoke-Monitor 'sendkey ret'
    [void](Wait-Regex "shell: command complete: $([regex]::Escape($Command))(?:\r?\n|$)" $offset $TimeoutSeconds)
    Pump-Serial 100
    $segment = $SerialText.ToString().Substring($offset)
    foreach ($pattern in $Expected) {
        if ($segment -notmatch $pattern) { throw "'$Command' missed expected '$pattern'." }
    }
    Test-Health
    return $segment
}

function Add-Pass {
    param([string]$Name, [string]$Evidence)
    $Results.Add([pscustomobject]@{ name = $Name; status = 'PASS'; evidence = $Evidence })
    Write-Host "[PASS] $Name - $Evidence" -ForegroundColor Green
}

try {
    Start-TestVm 'NVME BOOT' @(
        '-drive', "if=none,id=nvm,format=raw,file=$NvmeImage",
        '-device', 'nvme,drive=nvm,serial=TQNVME01,bootindex=1', '-nic', 'none'
    )
    [void](Wait-Regex 'BOOT: stage=entry COM1-ready' 0 120)
    [void](Wait-Regex 'BOOT: stage=shell-entered' 0 30)
    [void](Wait-Regex 'shell: console-ready mode=' 0 30)
    Add-Pass 'early boot diagnostics' 'COM1 recorded entry through shell stages before console dependence'
    [void](Wait-Regex 'storage: NVMe active nsid=1 sectors=[0-9]+ version=[0-9]+\.[0-9]+\.[0-9]+' 0 10)
    [void](Wait-Regex 'vfs: mounted TuwaiqFS v3 at /' 0 10)
    [void](Wait-Regex 'task heartbeat: beat #1' 0 30)
    [void](Wait-Regex 'task heartbeat: stable \(further beats every ~30s; not shell readiness\)' 0 30)
    Add-Pass 'NVMe boot and mount' '512-byte namespace identified; TuwaiqFS and scheduler reached live state'

    [void](Invoke-ShellCommand 'nvmetest' @('nvme-test: PASS invalid-lba malformed-namespace timeout-reset no-leak') 60)
    Add-Pass 'NVMe validation and recovery' 'invalid LBA and malformed identify data rejected; an injected command timeout required reset and retained claim/frame/heap baselines'
    [void](Invoke-ShellCommand 'write phase7b.txt nvme-persist' @('Wrote to: phase7b.txt') 60)
    [void](Invoke-ShellCommand 'cat phase7b.txt' @('nvme-persist') 30)
    Add-Pass 'NVMe write read and flush' 'TuwaiqFS checkpoint write flushed and reopened before reboot'

    Pump-Serial
    $rebootOffset = $SerialText.Length
    Send-Text 'reboot'
    Invoke-Monitor 'sendkey ret'
    [void](Wait-Regex 'BOOT: stage=entry COM1-ready' $rebootOffset 60)
    [void](Wait-Regex 'storage: NVMe active nsid=1 sectors=[0-9]+' $rebootOffset 60)
    [void](Wait-Regex 'task heartbeat: beat #1' $rebootOffset 90)
    [void](Invoke-ShellCommand 'cat phase7b.txt' @('nvme-persist') 30)
    Add-Pass 'NVMe reboot persistence' 'same NVMe image rebooted, remounted, and returned flushed data'
    Stop-TestVm

    Start-TestVm 'MALFORMED NAMESPACE' @(
        '-drive', "if=none,id=osdisk,format=raw,file=$AtaImage",
        '-device', 'ide-hd,drive=osdisk,bus=ide.0,unit=0,bootindex=1',
        '-drive', "if=none,id=badnvm,format=raw,file=$MalformedImage",
        '-device', 'nvme,drive=badnvm,serial=BADNS01,logical_block_size=4096,physical_block_size=4096', '-nic', 'none'
    )
    [void](Wait-Regex 'storage: NVMe rejected safely: NVMe has no active 512-byte namespace' 0 120)
    [void](Wait-Regex 'storage: NVMe failure cleanup active_claims=0' 0 10)
    [void](Wait-Regex 'storage: ATA PIO fallback active' 0 10)
    [void](Wait-Regex 'task heartbeat: beat #1' 0 60)
    Add-Pass 'malformed namespace cleanup' '4096-byte namespace rejected; claim released; ATA mounted without panic'
    Stop-TestVm

    Start-TestVm 'NVME ABSENT' @(
        '-drive', "if=none,id=osdisk,format=raw,file=$AtaImage",
        '-device', 'ide-hd,drive=osdisk,bus=ide.0,unit=0,bootindex=1',
        '-drive', "if=none,id=p7blk,format=raw,file=$VirtioImage",
        '-device', 'virtio-blk-pci,drive=p7blk,disable-modern=on,bootindex=2', '-nic', 'none'
    )
    [void](Wait-Regex 'storage: ATA PIO fallback active' 0 120)
    [void](Wait-Regex 'vfs: mounted TuwaiqFS v3 at /' 0 10)
    [void](Wait-Regex 'virtio-blk: PASS capacity=2048 sectors sector0-checksum=1874 teardown=reset\+scrub' 0 30)
    [void](Wait-Regex 'task heartbeat: beat #1' 0 60)
    if ($SerialText.ToString() -match 'storage: probing NVMe') { throw 'NVMe absent boot probed a nonexistent controller.' }
    Add-Pass 'NVMe absent ATA and VirtIO regression' 'ATA root and legacy VirtIO block remained intact with no NVMe function'
    Stop-TestVm

    Start-TestVm 'INPUT AND CONSOLE VALIDATION' @(
        '-machine', 'pc,i8042=off',
        '-drive', "if=none,id=osdisk,format=raw,file=$AtaImage",
        '-device', 'ide-hd,drive=osdisk,bus=ide.0,unit=0,bootindex=1',
        '-nic', 'none'
    )
    [void](Wait-Regex 'keyboard: PS/2 controller absent/unresponsive; boot continues' 0 120)
    [void](Wait-Regex 'mouse: IRQ12 remains masked; boot continues without mouse input' 0 30)
    [void](Wait-Regex 'BOOT-DEGRADE: component=ps2-keyboard' 0 30)
    [void](Wait-Regex 'framebuffer: validation self-test PASS' 0 30)
    [void](Wait-Regex 'BOOT: stage=shell-entered' 0 30)
    [void](Wait-Regex 'shell: console-ready mode=' 0 30)
    [void](Wait-Regex 'task heartbeat: beat #1' 0 60)
    Add-Pass 'input and console failure paths' 'absent 8042 remained non-fatal; malformed framebuffer geometry was rejected; COM1 reached shell and scheduler'
    Stop-TestVm

    $FatBroken = Join-Path $OutDir 'fat32-broken.img'
    Copy-Item -LiteralPath $AtaImage -Destination $FatBroken
    $fatBytes = [IO.File]::ReadAllBytes($FatBroken)
    # Destroy the packaged FAT32 boot-sector signature at LBA 24576 only.
    $fatBoot = 24576 * 512
    $fatBytes[$fatBoot + 510] = 0
    $fatBytes[$fatBoot + 511] = 0
    [IO.File]::WriteAllBytes($FatBroken, $fatBytes)

    Start-TestVm 'OPTIONAL DEVICE ABSENCE MATRIX' @(
        '-drive', "if=none,id=osdisk,format=raw,file=$FatBroken",
        '-device', 'ide-hd,drive=osdisk,bus=ide.0,unit=0,bootindex=1',
        '-nic', 'none'
    )
    [void](Wait-Regex 'BOOT: stage=entry COM1-ready' 0 120)
    [void](Wait-Regex 'vfs: mounted TuwaiqFS v3 at /' 0 60)
    [void](Wait-Regex 'vfs: FAT32 /boot mount failed:' 0 30)
    [void](Wait-Regex 'vfs: /boot UNAVAILABLE; boot continues' 0 30)
    [void](Wait-Regex 'BOOT-DEGRADE: component=fat32-boot' 0 30)
    [void](Wait-Regex 'BOOT-DEGRADE: component=virtio-net reason=device-absent fallback=network-offline' 0 30)
    [void](Wait-Regex 'BOOT: stage=shell-entered' 0 30)
    [void](Wait-Regex 'shell: console-ready mode=.* boot=UNAVAILABLE' 0 30)
    [void](Wait-Regex 'task heartbeat: beat #1' 0 60)
    if ($SerialText.ToString() -match 'KERNEL PANIC|fail: z') { throw 'Optional-device matrix panicked or hit bootloader fail:z.' }
    Add-Pass 'optional device absence matrix' 'invalid FAT32 and absent VirtIO net stayed non-fatal; shell/recovery markers reached over COM1'
    Test-Health
    Add-Pass 'kernel health' 'all scenarios completed without panic, double fault, deadlock, or unexpected exit'
    Stop-TestVm
    Save-Evidence
    Write-Host "Phase 7B NVMe smoke PASS: $OutDir" -ForegroundColor Green
} finally {
    try { Stop-TestVm } catch {}
    try { Save-Evidence } catch {}
}
