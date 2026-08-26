# Headless QEMU boot smoke test (PowerShell)
#
# Boots the built disk image with no display, waits for boot to settle,
# then uses the QEMU human monitor to grab a framebuffer screendump so
# boot success can be verified without a visible window (CI-friendly).
#
# This is a boot-reaches-shell check, not a full command regression suite.
# See docs/baseline/BASELINE_v0.5.md for the manual regression checklist
# this complements.

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot

$Candidates = @(
    (Join-Path $ProjectRoot "target\debug\boot-bios-tuwaiqos.img"),
    (Join-Path $ProjectRoot "target\release\boot-bios-tuwaiqos.img")
)
$Image = $Candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $Image) {
    Write-Host "Disk image not found. Run scripts\build.ps1 first." -ForegroundColor Red
    exit 1
}

$Qemu = Get-Command qemu-system-x86_64 -ErrorAction SilentlyContinue
if (-not $Qemu) {
    $FallbackQemu = "C:\Program Files\qemu\qemu-system-x86_64.exe"
    if (Test-Path $FallbackQemu) {
        $Qemu = $FallbackQemu
    } else {
        Write-Host "qemu-system-x86_64 not found in PATH or at $FallbackQemu." -ForegroundColor Red
        exit 1
    }
} else {
    $Qemu = $Qemu.Source
}

# -WindowStyle is a Windows-only Start-Process parameter; Unix PowerShell
# rejects it outright. QEMU is already headless (-display none) -- the style
# only hides the extra console window Windows would open for the child.
$HiddenWindowStyle = if ($PSVersionTable.PSVersion.Major -lt 6 -or $IsWindows) {
    @{ WindowStyle = 'Hidden' }
} else {
    @{}
}

$MonitorPort = 45465
$OutDir = Join-Path $ProjectRoot "target\smoke-test"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$Screenshot = Join-Path $OutDir "boot.ppm"
$SerialLog = Join-Path $OutDir "serial.log"
if (Test-Path $Screenshot) { Remove-Item $Screenshot -Force }
if (Test-Path $SerialLog) { Remove-Item $SerialLog -Force }

Write-Host "Booting $Image headlessly for smoke test..." -ForegroundColor Cyan

$proc = Start-Process -FilePath $Qemu -ArgumentList @(
    "-drive", "format=raw,file=$Image",
    "-m", "128M",
    "-display", "none",
    "-serial", "file:$SerialLog",
    "-monitor", "tcp:127.0.0.1:$MonitorPort,server,nowait",
    "-no-reboot"
) -PassThru @HiddenWindowStyle

Start-Sleep -Seconds 20

$client = New-Object System.Net.Sockets.TcpClient
$client.Connect("127.0.0.1", $MonitorPort)
$stream = $client.GetStream()
Start-Sleep -Milliseconds 500
$writer = New-Object System.IO.StreamWriter($stream)
$writer.AutoFlush = $true
$writer.WriteLine("screendump $Screenshot")
Start-Sleep -Seconds 2
$writer.WriteLine("quit")
Start-Sleep -Seconds 1
$writer.Close()
$client.Close()

if (-not $proc.HasExited) {
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
}

if (Test-Path $SerialLog) {
    Write-Host ""
    Write-Host "--- Serial log ---" -ForegroundColor Yellow
    Get-Content $SerialLog
    Write-Host "------------------" -ForegroundColor Yellow
}

if (Test-Path $Screenshot) {
    Write-Host "Screendump captured: $Screenshot" -ForegroundColor Green
    Write-Host "(Convert with ffmpeg -y -i `"$Screenshot`" -update 1 boot.png to view)"
    exit 0
} else {
    Write-Host "Screendump was not created — boot likely failed or hung." -ForegroundColor Red
    exit 1
}
