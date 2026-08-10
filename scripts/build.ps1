# Build TuwaiqOS (PowerShell)
#
# Output: target\debug\boot-bios-tuwaiqos.img

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot

$env:RUSTUP_TOOLCHAIN = "nightly-2026-06-01"
$env:CARGO_TARGET_DIR = Join-Path $ProjectRoot "target"

Write-Host "=== TuwaiqOS build ===" -ForegroundColor Cyan
Write-Host "Toolchain: $env:RUSTUP_TOOLCHAIN"

Write-Host ""
Write-Host "[1/3] Building all 36 userland ELF programs..." -ForegroundColor Yellow
# Standalone crate, own [workspace] -- see userland/hello/Cargo.toml. Must
# build before the kernel/image: shell.rs embeds explicit test fixtures and
# build.rs packages normal applications into TuwaiqFS. Sharing CARGO_TARGET_DIR
# keeps both consumers on the checked ELF artifacts. Invoked from *inside* the package directory
# deliberately -- cargo's config-file discovery walks up from the current
# working directory, not from --manifest-path, so running this from the
# repo root would silently miss userland/hello/.cargo/config.toml (the
# static-relocation/large-code-model/no-PIE flags a fixed high address
# like 0x700000000000 requires) and either mis-link as PIE or crash the
# linker outright.
Push-Location (Join-Path $ProjectRoot "userland\hello")
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    Pop-Location
}

$UserlandBins = @(
    "hello", "bad_syscall", "bad_pointer", "bad_privileged", "bad_kernel", "bad_unmapped",
    "bad_ud2", "bad_divzero", "bad_mmap", "bad_munmap", "bad_display", "bad_input",
    "mmap_ro_fault", "mmap_nx_fault", "post_unmap_fault", "mmap_exhaustion", "mmap_partial_failure",
    "desktop", "desktop_peer", "file_api_test", "file_mutation_test", "file_manager", "terminal",
    "tuwaiq_ai", "tuwaiq_ai_fault", "ipc_provider", "ipc_client", "ipc_intruder",
    "ipc_crash_provider", "ipc_crash_client", "ipc_timeout_provider", "ipc_timeout_client",
    "ipc_backpressure_provider", "ipc_backpressure_client", "ipc_waiter", "bad_ipc"
)
foreach ($bin in $UserlandBins) {
    $path = Join-Path $ProjectRoot "target\x86_64-unknown-none\release\$bin"
    if (-not (Test-Path $path)) {
        Write-Host "userland binary '$bin' not found after step 1." -ForegroundColor Red
        exit 1
    }
}
Write-Host "userland binaries: $($UserlandBins -join ', ')" -ForegroundColor Green

Write-Host ""
Write-Host "[2/3] Building bare-metal kernel..." -ForegroundColor Yellow
cargo build --package kernel --target x86_64-unknown-none
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$KernelElf = Join-Path $ProjectRoot "target\x86_64-unknown-none\debug\kernel"
if (-not (Test-Path $KernelElf)) {
    $KernelElf = Join-Path $ProjectRoot "target\x86_64-unknown-none\debug\kernel.exe"
}
if (-not (Test-Path $KernelElf)) {
    Write-Host "Kernel ELF not found after step 2." -ForegroundColor Red
    exit 1
}
Write-Host "Kernel ELF: $KernelElf" -ForegroundColor Green

Write-Host ""
Write-Host "[3/3] Building BIOS disk image..." -ForegroundColor Yellow
cargo build --package tuwaiqos
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$Image = Join-Path $ProjectRoot "target\debug\boot-bios-tuwaiqos.img"
if (-not (Test-Path $Image)) {
    Write-Host "Disk image not found at expected path." -ForegroundColor Red
    Get-ChildItem -Recurse (Join-Path $ProjectRoot "target") -Filter "boot-bios-tuwaiqos.img" -ErrorAction SilentlyContinue
    exit 1
}

Write-Host ""
Write-Host "Build complete." -ForegroundColor Green
Write-Host "Disk image: $Image"
Write-Host "Size: $((Get-Item $Image).Length) bytes"
