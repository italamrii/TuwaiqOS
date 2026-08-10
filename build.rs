//! Host-side build script (runs on Windows during `cargo build -p tuwaiqos`).
//!
//! This script ONLY wraps an already-built kernel ELF into a BIOS disk image.
//! The kernel is built separately first (see `scripts/build.ps1`) so we do not
//! spawn nested `cargo` while the `bootloader` crate's own build.rs is running.

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const KERNEL_TARGET: &str = "x86_64-unknown-none";
const KERNEL_BIN: &str = "kernel";
const IMAGE_NAME: &str = "boot-bios-tuwaiqos.img";
const SECTOR_SIZE: u64 = 512;
const FAT32_START_LBA: u32 = 24_576;
const FAT32_TOTAL_SECTORS: u32 = 69_632;
const FAT32_RESERVED_SECTORS: u32 = 32;
const FAT32_FAT_SECTORS: u32 = 536;
const TUWAIQFS_SUPERBLOCK_LBA: u32 = 8192;
const TUWAIQFS_SLOT_A_LBA: u32 = 23_490;
const TUWAIQFS_SLOT_SECTORS: u32 = 512;
const TUWAIQFS_SLOT_B_LBA: u32 = TUWAIQFS_SLOT_A_LBA + TUWAIQFS_SLOT_SECTORS;
const TUWAIQFS_MAX_METADATA: usize = (TUWAIQFS_SLOT_SECTORS as usize - 1) * 512;
const TUWAIQFS_SLOT_COMMITTED: u32 = 0x434F_4D54;

fn main() {
    if let Err(err) = run() {
        eprintln!();
        eprintln!("=== TuwaiqOS image build FAILED ===");
        eprintln!("{err}");
        eprintln!("===================================");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    track_kernel_sources();
    track_application_sources();
    print_build_environment();

    let profile = env_var("PROFILE")?;
    let manifest_dir = PathBuf::from(env_var("CARGO_MANIFEST_DIR")?);
    let target_dir = env_var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest_dir.join("target"));

    let kernel_elf = locate_kernel_elf(&manifest_dir, &target_dir, &profile)
        .ok_or_else(|| kernel_not_found_message(&manifest_dir, &target_dir, &profile))?;
    println!("cargo:rerun-if-changed={}", kernel_elf.display());

    let image_dir = target_dir.join(&profile);
    let image = image_dir.join(IMAGE_NAME);

    print_section("Paths");
    print_kv("manifest_dir", &manifest_dir);
    print_kv("target_dir", &target_dir);
    print_str("profile", &profile);
    print_kv("kernel_elf", &kernel_elf);
    print_kv("output_image", &image);

    print_section("Pre-flight checks");
    verify_kernel_elf(&kernel_elf)?;
    ensure_output_dir(&image_dir)?;
    verify_llvm_objcopy()?;

    print_section("Creating BIOS disk image");
    println!("Calling bootloader::BiosBoot::create_disk_image(...)");

    match bootloader::BiosBoot::new(&kernel_elf).create_disk_image(&image) {
        Ok(()) => {
            println!("create_disk_image returned Ok");
        }
        Err(err) => {
            return Err(format!(
                "bootloader::BiosBoot::create_disk_image failed:\n{err:#}"
            ));
        }
    }

    if !image.exists() {
        return Err(format!(
            "create_disk_image reported success but file is missing:\n  {}",
            image.display()
        ));
    }

    create_tuwaiqfs_application_volume(&image, &target_dir)?;
    create_fat32_resource_volume(&image)?;

    let image_size = fs::metadata(&image)
        .map_err(|e| format!("cannot read image metadata: {e}"))?
        .len();

    print_section("Success");
    println!("Disk image : {}", image.display());
    println!("Image size : {image_size} bytes");

    println!("cargo:rustc-env=TUWAIQOS_BOOT_IMAGE={}", image.display());
    println!("cargo:warning=TuwaiqOS disk image: {}", image.display());

    Ok(())
}

fn create_tuwaiqfs_application_volume(image: &Path, target_dir: &Path) -> Result<(), String> {
    let applications = [
        ("desktop", "desktop"),
        ("file_manager", "file-manager"),
        ("terminal", "terminal"),
        ("tuwaiq_ai", "tuwaiq-ai"),
        ("ipc_provider", "ipc-provider"),
        ("ipc_client", "ipc-client"),
        ("ipc_intruder", "ipc-intruder"),
        ("ipc_crash_provider", "ipc-crash-provider"),
        ("ipc_crash_client", "ipc-crash-client"),
        ("ipc_timeout_provider", "ipc-timeout-provider"),
        ("ipc_timeout_client", "ipc-timeout-client"),
        ("ipc_backpressure_provider", "ipc-backpressure-provider"),
        ("ipc_backpressure_client", "ipc-backpressure-client"),
        ("ipc_waiter", "ipc-waiter"),
        ("bad_ipc", "bad-ipc"),
    ];
    let mut blob = Vec::new();
    blob.extend_from_slice(b"TREE");
    write_tuwaiqfs_record(&mut blob, 2, "apps", None)?;
    for (binary, installed_name) in applications {
        let path = target_dir
            .join("x86_64-unknown-none")
            .join("release")
            .join(binary);
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = fs::read(&path).map_err(|error| {
            format!(
                "packaged application '{}' is missing; run scripts\\build.ps1: {error}",
                path.display()
            )
        })?;
        let installed_path = format!("apps/{installed_name}");
        write_tuwaiqfs_record(&mut blob, 1, &installed_path, Some(&bytes))?;
    }
    let catalog = b"desktop\nfile-manager\nterminal\ntuwaiq-ai\nipc-provider\nipc-client\nipc-intruder\nipc-crash-provider\nipc-crash-client\nipc-timeout-provider\nipc-timeout-client\nipc-backpressure-provider\nipc-backpressure-client\nipc-waiter\nbad-ipc\n";
    write_tuwaiqfs_record(&mut blob, 1, "apps/catalog.txt", Some(catalog))?;
    write_tuwaiqfs_record(&mut blob, 2, "data", None)?;
    for directory in [
        "desktop",
        "file-manager",
        "terminal",
        "tuwaiq-ai",
        "ipc-provider",
        "ipc-client",
        "ipc-intruder",
        "ipc-crash-provider",
        "ipc-crash-client",
        "ipc-timeout-provider",
        "ipc-timeout-client",
        "ipc-backpressure-provider",
        "ipc-backpressure-client",
        "ipc-waiter",
        "bad-ipc",
    ] {
        write_tuwaiqfs_record(&mut blob, 2, &format!("data/{directory}"), None)?;
    }
    if blob.len() > TUWAIQFS_MAX_METADATA {
        return Err(format!(
            "packaged TuwaiqFS metadata is {} bytes, limit is {}",
            blob.len(),
            TUWAIQFS_MAX_METADATA
        ));
    }

    let mut superblock = [0u8; 512];
    superblock[..8].copy_from_slice(b"TQFSv2\0\0");
    superblock[8..12].copy_from_slice(&3u32.to_le_bytes());
    superblock[12..16].copy_from_slice(&TUWAIQFS_SLOT_A_LBA.to_le_bytes());
    superblock[16..20].copy_from_slice(&TUWAIQFS_SLOT_SECTORS.to_le_bytes());

    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(image)
        .map_err(|error| format!("cannot open disk image for TuwaiqFS setup: {error}"))?;
    write_sector(&mut file, TUWAIQFS_SUPERBLOCK_LBA, &superblock)?;
    let mut checkpoint = [0u8; 512];
    checkpoint[..8].copy_from_slice(b"TQCKPT\0\0");
    checkpoint[8..16].copy_from_slice(&1u64.to_le_bytes());
    checkpoint[16..20].copy_from_slice(&(blob.len() as u32).to_le_bytes());
    checkpoint[20..24].copy_from_slice(&crc32(&blob).to_le_bytes());
    write_sector(&mut file, TUWAIQFS_SLOT_A_LBA, &checkpoint)?;
    write_sector(&mut file, TUWAIQFS_SLOT_B_LBA, &[0u8; 512])?;
    let mut sector = [0u8; 512];
    let mut offset = 0usize;
    for index in 0..blob.len().div_ceil(512) {
        sector.fill(0);
        let count = blob.len().saturating_sub(offset).min(512);
        if count != 0 {
            sector[..count].copy_from_slice(&blob[offset..offset + count]);
            offset += count;
        }
        write_sector(&mut file, TUWAIQFS_SLOT_A_LBA + 1 + index as u32, &sector)?;
    }
    checkpoint[24..28].copy_from_slice(&TUWAIQFS_SLOT_COMMITTED.to_le_bytes());
    write_sector(&mut file, TUWAIQFS_SLOT_A_LBA, &checkpoint)?;
    println!(
        "TuwaiqFS application volume: {} bytes, {} packaged applications",
        blob.len(),
        applications.len()
    );
    Ok(())
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn write_tuwaiqfs_record(
    out: &mut Vec<u8>,
    kind: u8,
    path: &str,
    content: Option<&[u8]>,
) -> Result<(), String> {
    if path.is_empty() || path.len() > 120 || path.len() > u8::MAX as usize {
        return Err(format!("invalid packaged TuwaiqFS path: {path}"));
    }
    out.push(kind);
    out.push(path.len() as u8);
    out.extend_from_slice(path.as_bytes());
    if kind == 1 {
        let bytes = content.ok_or_else(|| format!("missing content for {path}"))?;
        let length = u16::try_from(bytes.len())
            .map_err(|_| format!("packaged application exceeds 65,535 bytes: {path}"))?;
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(bytes);
    }
    Ok(())
}

fn create_fat32_resource_volume(image: &Path) -> Result<(), String> {
    let final_len = u64::from(FAT32_START_LBA + FAT32_TOTAL_SECTORS) * SECTOR_SIZE;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(image)
        .map_err(|error| format!("cannot open disk image for FAT32 setup: {error}"))?;
    file.set_len(final_len)
        .map_err(|error| format!("cannot extend disk image for FAT32: {error}"))?;

    let mut partition = [0u8; 16];
    partition[4] = 0x0C;
    partition[8..12].copy_from_slice(&FAT32_START_LBA.to_le_bytes());
    partition[12..16].copy_from_slice(&FAT32_TOTAL_SECTORS.to_le_bytes());
    write_at(&mut file, 446 + 2 * 16, &partition)?;

    let mut boot = [0u8; 512];
    boot[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    boot[3..11].copy_from_slice(b"TUWAIQOS");
    boot[11..13].copy_from_slice(&512u16.to_le_bytes());
    boot[13] = 1;
    boot[14..16].copy_from_slice(&(FAT32_RESERVED_SECTORS as u16).to_le_bytes());
    boot[16] = 2;
    boot[21] = 0xF8;
    boot[24..26].copy_from_slice(&32u16.to_le_bytes());
    boot[26..28].copy_from_slice(&64u16.to_le_bytes());
    boot[28..32].copy_from_slice(&FAT32_START_LBA.to_le_bytes());
    boot[32..36].copy_from_slice(&FAT32_TOTAL_SECTORS.to_le_bytes());
    boot[36..40].copy_from_slice(&FAT32_FAT_SECTORS.to_le_bytes());
    boot[44..48].copy_from_slice(&2u32.to_le_bytes());
    boot[48..50].copy_from_slice(&1u16.to_le_bytes());
    boot[50..52].copy_from_slice(&6u16.to_le_bytes());
    boot[64] = 0x80;
    boot[66] = 0x29;
    boot[67..71].copy_from_slice(&0x5451_4653u32.to_le_bytes());
    boot[71..82].copy_from_slice(b"TUWAIQBOOT ");
    boot[82..90].copy_from_slice(b"FAT32   ");
    boot[510] = 0x55;
    boot[511] = 0xAA;
    write_sector(&mut file, FAT32_START_LBA, &boot)?;
    write_sector(&mut file, FAT32_START_LBA + 6, &boot)?;

    let mut fsinfo = [0u8; 512];
    fsinfo[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    fsinfo[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    fsinfo[488..492].copy_from_slice(&u32::MAX.to_le_bytes());
    fsinfo[492..496].copy_from_slice(&u32::MAX.to_le_bytes());
    fsinfo[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
    write_sector(&mut file, FAT32_START_LBA + 1, &fsinfo)?;

    let mut fat = [0u8; 512];
    for (cluster, value) in [
        (0usize, 0x0FFF_FFF8u32),
        (1, 0xFFFF_FFFF),
        (2, 0x0FFF_FFFF),
        (3, 0x0FFF_FFFF),
        (4, 0x0FFF_FFFF),
        (5, 0x0FFF_FFFF),
    ] {
        let offset = cluster * 4;
        fat[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    let first_fat = FAT32_START_LBA + FAT32_RESERVED_SECTORS;
    write_sector(&mut file, first_fat, &fat)?;
    write_sector(&mut file, first_fat + FAT32_FAT_SECTORS, &fat)?;

    let data_start = FAT32_START_LBA + FAT32_RESERVED_SECTORS + 2 * FAT32_FAT_SECTORS;
    let readme = b"TuwaiqOS FAT32 resource volume\n";
    let apps = b"desktop\nfile-manager\nterminal\n";
    let mut root = [0u8; 512];
    write_short_entry(
        &mut root[0..32],
        b"README  TXT",
        0x20,
        3,
        readme.len() as u32,
    );
    write_short_entry(&mut root[32..64], b"DOCS       ", 0x10, 4, 0);
    write_sector(&mut file, data_start, &root)?;

    let mut readme_sector = [0u8; 512];
    readme_sector[..readme.len()].copy_from_slice(readme);
    write_sector(&mut file, data_start + 1, &readme_sector)?;

    let mut docs = [0u8; 512];
    write_short_entry(&mut docs[0..32], b".          ", 0x10, 4, 0);
    write_short_entry(&mut docs[32..64], b"..         ", 0x10, 2, 0);
    write_short_entry(
        &mut docs[64..96],
        b"APPS    TXT",
        0x20,
        5,
        apps.len() as u32,
    );
    write_sector(&mut file, data_start + 2, &docs)?;

    let mut apps_sector = [0u8; 512];
    apps_sector[..apps.len()].copy_from_slice(apps);
    write_sector(&mut file, data_start + 3, &apps_sector)?;
    println!("FAT32 resource volume: LBA {FAT32_START_LBA}, {FAT32_TOTAL_SECTORS} sectors");
    Ok(())
}

fn write_short_entry(out: &mut [u8], name: &[u8; 11], attributes: u8, cluster: u32, size: u32) {
    out[..11].copy_from_slice(name);
    out[11] = attributes;
    out[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    out[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    out[28..32].copy_from_slice(&size.to_le_bytes());
}

fn write_sector(file: &mut fs::File, lba: u32, bytes: &[u8; 512]) -> Result<(), String> {
    write_at(file, u64::from(lba) * SECTOR_SIZE, bytes)
}

fn write_at(file: &mut fs::File, offset: u64, bytes: &[u8]) -> Result<(), String> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| format!("disk image seek failed at {offset}: {error}"))?;
    file.write_all(bytes)
        .map_err(|error| format!("disk image write failed at {offset}: {error}"))
}

/// Track every kernel source file, not a hand-maintained subset.
///
/// A hardcoded file list here previously omitted several modules (this is
/// how, during Phase 1 interrupt work, `gdt.rs`/`interrupts.rs`/`serial.rs`
/// went unlisted): Cargo saw nothing *this build script itself watches*
/// had changed and skipped rerunning it, silently repackaging a stale
/// kernel ELF into the disk image for several rebuild-and-test cycles.
/// Walking the actual directory tree makes that class of bug structurally
/// impossible -- a new module is tracked the moment its file exists.
fn track_kernel_sources() {
    println!("cargo:rerun-if-changed=kernel/Cargo.toml");
    let mut dirs = std::collections::VecDeque::new();
    dirs.push_back(PathBuf::from("kernel/src"));
    while let Some(dir) = dirs.pop_front() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push_back(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
}

fn track_application_sources() {
    println!("cargo:rerun-if-changed=userland/hello/Cargo.toml");
    let mut dirs = std::collections::VecDeque::new();
    dirs.push_back(PathBuf::from("userland/hello/src"));
    while let Some(dir) = dirs.pop_front() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push_back(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
}

fn print_build_environment() {
    print_section("Build environment");

    print_kv_optional("RUSTUP_TOOLCHAIN", option_env("RUSTUP_TOOLCHAIN"));
    print_kv_optional("RUSTUP_HOME", option_env("RUSTUP_HOME"));
    print_kv_optional("CARGO", option_env("CARGO"));
    print_kv_optional("RUSTC", option_env("RUSTC"));
    print_kv_optional("PROFILE", option_env("PROFILE"));
    print_kv_optional("CARGO_MANIFEST_DIR", option_env("CARGO_MANIFEST_DIR"));
    print_kv_optional("CARGO_TARGET_DIR", option_env("CARGO_TARGET_DIR"));

    if let Ok(output) = Command::new("rustup")
        .args(["show", "active-toolchain"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !text.is_empty() {
            println!("active_toolchain : {text}");
        }
    }

    if let Ok(output) = Command::new("rustup").args(["which", "rustc"]).output() {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !text.is_empty() {
            println!("rustup which rustc : {text}");
        }
    }
}

fn verify_kernel_elf(path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(path).map_err(|e| {
        format!(
            "kernel ELF does not exist or is unreadable:\n  {}\n  {e}",
            path.display()
        )
    })?;

    if !metadata.is_file() {
        return Err(format!("kernel path is not a file:\n  {}", path.display()));
    }

    if metadata.len() == 0 {
        return Err(format!("kernel ELF is empty:\n  {}", path.display()));
    }

    println!("kernel ELF exists ({} bytes)", metadata.len());
    Ok(())
}

fn ensure_output_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| {
        format!(
            "failed to create output directory:\n  {}\n  {e}",
            path.display()
        )
    })?;
    println!("output directory ready: {}", path.display());
    Ok(())
}

fn verify_llvm_objcopy() -> Result<(), String> {
    match llvm_tools::LlvmTools::new() {
        Ok(tools) => match tools.tool(&llvm_tools::exe("llvm-objcopy")) {
            Some(path) => {
                println!("llvm-objcopy : {}", path.display());
                Ok(())
            }
            None => Err("llvm-objcopy not found in llvm-tools-preview.\n  \
                 Run: rustup component add llvm-tools-preview --toolchain nightly-2026-06-01"
                .to_string()),
        },
        Err(err) => Err(format!(
            "failed to initialize llvm-tools:\n  {err:?}\n  \
             Run: rustup component add llvm-tools-preview --toolchain nightly-2026-06-01"
        )),
    }
}

fn locate_kernel_elf(manifest_dir: &Path, target_dir: &Path, profile: &str) -> Option<PathBuf> {
    let candidates = [
        target_dir
            .join(KERNEL_TARGET)
            .join(profile)
            .join(KERNEL_BIN),
        target_dir
            .join(KERNEL_TARGET)
            .join(profile)
            .join(format!("{KERNEL_BIN}.exe")),
        manifest_dir
            .join("target")
            .join(KERNEL_TARGET)
            .join(profile)
            .join(KERNEL_BIN),
        manifest_dir
            .join("target")
            .join(KERNEL_TARGET)
            .join(profile)
            .join(format!("{KERNEL_BIN}.exe")),
    ];

    print_section("Kernel ELF search");
    for candidate in &candidates {
        let status = if candidate.exists() {
            "FOUND"
        } else {
            "missing"
        };
        println!("  [{status}] {}", candidate.display());
    }

    candidates.into_iter().find(|path| path.exists())
}

fn kernel_not_found_message(manifest_dir: &Path, target_dir: &Path, profile: &str) -> String {
    format!(
        "kernel ELF not found.\n\n\
         Build the kernel FIRST, then build the disk image:\n\n\
           cargo build --package kernel --target {KERNEL_TARGET}\n\
           cargo build --package tuwaiqos\n\n\
         Or use the helper script:\n\n\
           .\\scripts\\build.ps1\n\n\
         Expected locations (profile={profile}):\n\
           {}\n\
           {}\n\
         manifest_dir: {}\n\
         target_dir  : {}",
        target_dir
            .join(KERNEL_TARGET)
            .join(profile)
            .join(KERNEL_BIN)
            .display(),
        manifest_dir
            .join("target")
            .join(KERNEL_TARGET)
            .join(profile)
            .join(KERNEL_BIN)
            .display(),
        manifest_dir.display(),
        target_dir.display(),
    )
}

fn env_var(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("missing environment variable: {name}"))
}

fn option_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn print_section(title: &str) {
    println!();
    println!("--- {title} ---");
}

fn print_kv(label: &str, path: &Path) {
    println!("{label:16}: {}", path.display());
}

fn print_str(label: &str, value: &str) {
    println!("{label:16}: {value}");
}

fn print_kv_optional(label: &str, value: Option<String>) {
    match value {
        Some(v) => println!("{label:16}: {v}"),
        None => println!("{label:16}: (not set)"),
    }
}
