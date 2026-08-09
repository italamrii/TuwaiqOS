# Build a VMware Workstation / ESXi-oriented BIOS artifact from the normal raw image.
#
# Primary tester artifact: target\esxi\TuwaiqOS-VMware-BIOS.vmdk
#   single-file monolithicSparse VMDK, attachable without descriptor edits.
#
# Release-engineering companions (not required for a focused external boot test):
#   TuwaiqOS-VMware-BIOS.vmx, SHA256SUMS.txt, README-VMware.md
# Optional ESXi transport (not for direct Workstation attach):
#   TuwaiqOS-ESXi-Transport.vmdk (streamOptimized; convert with vmkfstools on ESXi)

[CmdletBinding()]
param(
    [ValidateRange(13, 21)]
    [int]$VirtualHardwareVersion = 13,
    [string]$OutputDirectory,
    [string]$QemuImg,
    [switch]$SkipBuild,
    [switch]$SkipTransport
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$ProjectRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $ProjectRoot 'target\esxi'
}
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$AllowedOutputRoot = [IO.Path]::GetFullPath((Join-Path $ProjectRoot 'target'))
if (-not $OutputDirectory.StartsWith($AllowedOutputRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw "VMware output must remain below '$AllowedOutputRoot'."
}

if (-not $QemuImg) {
    $command = Get-Command qemu-img -ErrorAction SilentlyContinue
    if ($command) {
        $QemuImg = $command.Source
    } else {
        $QemuImg = Join-Path $env:ProgramFiles 'qemu\qemu-img.exe'
    }
}
$QemuImg = [IO.Path]::GetFullPath($QemuImg)
if (-not (Test-Path -LiteralPath $QemuImg -PathType Leaf)) {
    throw "qemu-img was not found. Pass -QemuImg with the installed executable path."
}

# Query this installed binary rather than assuming its VMDK implementation.
$VmdkOptions = (& $QemuImg create -f vmdk -o help 2>&1 | Out-String)
if ($LASTEXITCODE -ne 0) { throw "qemu-img could not enumerate VMDK options." }
foreach ($required in @('hwversion=', 'adapter_type=', 'subformat=')) {
    if ($VmdkOptions -notmatch [regex]::Escape($required)) {
        throw "Installed qemu-img lacks required VMDK option '$required'."
    }
}
if ($VmdkOptions -notmatch 'monolithicSparse') {
    throw "Installed qemu-img lacks monolithicSparse VMDK subformat."
}
if (-not $SkipTransport -and $VmdkOptions -notmatch 'streamOptimized') {
    throw "Installed qemu-img lacks streamOptimized VMDK subformat; pass -SkipTransport to omit it."
}

if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build.ps1')
    if ($LASTEXITCODE -ne 0) { throw 'Normal TuwaiqOS build failed.' }
}

$RawImage = Join-Path $ProjectRoot 'target\debug\boot-bios-tuwaiqos.img'
if (-not (Test-Path -LiteralPath $RawImage -PathType Leaf)) {
    throw "Raw BIOS image is missing at '$RawImage'."
}

[void](New-Item -ItemType Directory -Force -Path $OutputDirectory)
# Copy first so a concurrent QEMU open of the live image cannot race conversion.
$RawSnapshot = Join-Path $OutputDirectory 'source-boot-bios-tuwaiqos.img'
Copy-Item -LiteralPath $RawImage -Destination $RawSnapshot -Force
$RawHashBefore = (Get-FileHash -Algorithm SHA256 -LiteralPath $RawSnapshot).Hash
$RawLength = (Get-Item -LiteralPath $RawSnapshot).Length
if ($RawLength -lt 1MB -or ($RawLength % 512) -ne 0) {
    throw "Raw BIOS image has invalid size $RawLength."
}
$RawInfo = (& $QemuImg info --output=json $RawSnapshot | ConvertFrom-Json)
if ($LASTEXITCODE -ne 0 -or $RawInfo.format -ne 'raw' -or [int64]$RawInfo.'virtual-size' -ne $RawLength) {
    throw 'qemu-img info did not validate the source raw BIOS image.'
}
$LiveHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $RawImage).Hash
if ($LiveHash -ne $RawHashBefore) {
    throw 'Source raw image changed while creating the conversion snapshot.'
}

$PrimaryVmdkName = 'TuwaiqOS-VMware-BIOS.vmdk'
$TransportVmdkName = 'TuwaiqOS-ESXi-Transport.vmdk'
$VmxName = 'TuwaiqOS-VMware-BIOS.vmx'
$ChecksumName = 'SHA256SUMS.txt'
$ReadmeName = 'README-VMware.md'

$PrimaryVmdk = Join-Path $OutputDirectory $PrimaryVmdkName
$TransportVmdk = Join-Path $OutputDirectory $TransportVmdkName
$Vmx = Join-Path $OutputDirectory $VmxName
$Checksums = Join-Path $OutputDirectory $ChecksumName
$Readme = Join-Path $OutputDirectory $ReadmeName

# Remove prior Phase 7B names so the directory cannot present ambiguous VMDKs.
$LegacyNames = @(
    'TuwaiqOS-ESXi.vmdk',
    'TuwaiqOS-ESXi.vmx',
    'TuwaiqOS-ESXi-Native.vmdk',
    'TuwaiqOS-Phase7-ESXi.vmdk',
    'TuwaiqOS-Phase7-ESXi.sha256.txt',
    'README-ESXi.md',
    $PrimaryVmdkName,
    $TransportVmdkName,
    $VmxName,
    $ChecksumName,
    $ReadmeName
)
foreach ($name in $LegacyNames) {
    $path = Join-Path $OutputDirectory $name
    if (Test-Path -LiteralPath $path) {
        Remove-Item -LiteralPath $path -Force
    }
}
# monolithicFlat leaves a sibling "*-flat.vmdk"; purge probe leftovers too.
Get-ChildItem -LiteralPath $OutputDirectory -Filter '*-flat.vmdk' -ErrorAction SilentlyContinue |
    Remove-Item -Force

function Convert-AndValidateVmdk {
    param(
        [Parameter(Mandatory)][string]$Destination,
        [Parameter(Mandatory)][string]$Subformat,
        [Parameter(Mandatory)][string]$AdapterType
    )
    $options = "subformat=$Subformat,adapter_type=$AdapterType,hwversion=$VirtualHardwareVersion"
    & $QemuImg convert -f raw -O vmdk -o $options $RawSnapshot $Destination
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $Destination -PathType Leaf)) {
        throw "qemu-img VMDK conversion failed for $Destination ($Subformat)."
    }
    $info = (& $QemuImg info --output=json $Destination | ConvertFrom-Json)
    if ($LASTEXITCODE -ne 0 -or $info.format -ne 'vmdk' -or [int64]$info.'virtual-size' -ne $RawLength) {
        throw "qemu-img info did not validate VMDK geometry for $Destination."
    }
    $createType = [string]$info.'format-specific'.data.'create-type'
    if ($createType -ne $Subformat) {
        throw "VMDK create type '$createType' does not match requested '$Subformat'."
    }
    $check = (& $QemuImg check --output=json $Destination | ConvertFrom-Json)
    if ($LASTEXITCODE -ne 0 -or [int64]$check.'check-errors' -ne 0) {
        throw "qemu-img check found VMDK errors in $Destination."
    }
    return [pscustomobject]@{
        Path = $Destination
        Subformat = $Subformat
        VirtualSize = [int64]$info.'virtual-size'
        CheckErrors = [int64]$check.'check-errors'
        Sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $Destination).Hash.ToLowerInvariant()
        Length = (Get-Item -LiteralPath $Destination).Length
    }
}

# Workstation/ESXi direct-attach path: single-file sparse VMDK. streamOptimized
# is transport-only and was rejected or stalled earlier by Workstation.
$Primary = Convert-AndValidateVmdk -Destination $PrimaryVmdk -Subformat 'monolithicSparse' -AdapterType 'lsilogic'
$Transport = $null
if (-not $SkipTransport) {
    $Transport = Convert-AndValidateVmdk -Destination $TransportVmdk -Subformat 'streamOptimized' -AdapterType 'lsilogic'
}

$VmxText = @"
.encoding = "UTF-8"
config.version = "8"
virtualHW.version = "$VirtualHardwareVersion"
displayName = "TuwaiqOS Phase 7B"
guestOS = "other-64"
firmware = "bios"
numvcpus = "1"
memSize = "512"
cpuid.coresPerSocket = "1"
bios.bootOrder = "hdd"
nvme0.present = "TRUE"
nvme0:0.present = "TRUE"
nvme0:0.fileName = "$PrimaryVmdkName"
nvme0:0.redo = ""
ethernet0.present = "FALSE"
usb.present = "TRUE"
usb_xhci.present = "FALSE"
floppy0.present = "FALSE"
serial0.present = "TRUE"
serial0.fileType = "file"
serial0.fileName = "TuwaiqOS-VMware-BIOS-COM1.log"
serial0.yieldOnMsrRead = "TRUE"
mks.enable3d = "FALSE"
"@
[IO.File]::WriteAllText($Vmx, $VmxText, [Text.UTF8Encoding]::new($false))

$ReadmeTemplate = Get-Content -LiteralPath (Join-Path $ProjectRoot 'docs\ESXI.md') -Raw
$ReadmeText = $ReadmeTemplate.Replace('{{VIRTUAL_HW_VERSION}}', [string]$VirtualHardwareVersion)
$ReadmeText = $ReadmeText.Replace('{{PRIMARY_VMDK}}', $PrimaryVmdkName)
$ReadmeText = $ReadmeText.Replace('{{TRANSPORT_VMDK}}', $TransportVmdkName)
[IO.File]::WriteAllText($Readme, $ReadmeText, [Text.UTF8Encoding]::new($false))

$RawHashAfter = (Get-FileHash -Algorithm SHA256 -LiteralPath $RawSnapshot).Hash
if ($RawHashAfter -ne $RawHashBefore) {
    throw 'The source raw snapshot changed during VMware conversion.'
}
$LiveHashAfter = (Get-FileHash -Algorithm SHA256 -LiteralPath $RawImage).Hash
if ($LiveHashAfter -ne $RawHashBefore) {
    throw 'The live source raw image changed during VMware conversion.'
}

$ChecksumArtifacts = @($PrimaryVmdk, $Vmx, $Readme)
if ($null -ne $Transport) { $ChecksumArtifacts += $TransportVmdk }
$ChecksumLines = foreach ($artifact in $ChecksumArtifacts) {
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $artifact).Hash.ToLowerInvariant()
    "$hash  $([IO.Path]::GetFileName($artifact))"
}
$ChecksumLines += "$($RawHashAfter.ToLowerInvariant())  source/boot-bios-tuwaiqos.img"
[IO.File]::WriteAllLines($Checksums, $ChecksumLines, [Text.UTF8Encoding]::new($false))
Remove-Item -LiteralPath $RawSnapshot -Force

Write-Host 'VMware artifact PASS' -ForegroundColor Green
Write-Host "  Primary VMDK (tester): $PrimaryVmdk"
Write-Host "    subformat=$($Primary.Subformat) bytes=$($Primary.Length) sha256=$($Primary.Sha256)"
if ($null -ne $Transport) {
    Write-Host "  Transport VMDK (optional): $TransportVmdk"
    Write-Host "    subformat=$($Transport.Subformat) bytes=$($Transport.Length) sha256=$($Transport.Sha256)"
}
Write-Host "  VMX (optional template): $Vmx"
Write-Host "  SHA256SUMS (integrity only): $Checksums"
Write-Host "  README: $Readme"
Write-Host "  source raw unchanged: $RawHashAfter virtual-size=$RawLength"
Write-Host '  Note: SHA-256 verifies transfer integrity; it does not prove boot compatibility.'
