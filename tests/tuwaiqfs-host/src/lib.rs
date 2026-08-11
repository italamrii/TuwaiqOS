//! Host-side test rig for TuwaiqFS.
//!
//! The kernel's own `tuwaiqfs.rs` and `fs.rs` are compiled into this crate
//! verbatim (see `build.rs`). Only the two kernel services they call out to
//! are replaced:
//!
//! - `crate::ata` — a sparse in-memory disk instead of PIO port I/O
//! - `serial_println!` — a sink instead of the UART
//!
//! No filesystem logic is reimplemented here, so these tests cannot pass
//! against a copy that has drifted away from the kernel.
//!
//! ## Running
//!
//! ```text
//! cd tests/tuwaiqfs-host
//! cargo test
//! ```
//!
//! Tests that touch the kernel's global filesystem state (`fs.rs` keeps a
//! `static mut FS`) must hold [`TEST_LOCK`] for their duration — see
//! [`with_fresh_fs`].

// `tuwaiqfs.rs` and `fs.rs` are `no_std` and import from `alloc`
// (`use alloc::string::String;` and friends). Aliasing `std` to that name
// makes those lines resolve unchanged, and `std` re-exports every module the
// kernel uses -- `string`, `vec`, `boxed`, `collections`.
//
// Deliberately NOT `extern crate alloc;`: the workspace `.cargo/config.toml`
// enables `-Z build-std` for the bare-metal target, which builds `alloc` from
// source. Linking that alongside the `alloc` that `std` already carries is a
// duplicate-lang-item error (`owned_box`). This alias sidesteps it without
// touching the kernel's build configuration.
extern crate std as alloc;

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Serialises tests that touch kernel global state.
///
/// `fs.rs` stores the mounted filesystem in a `static mut`, and the simulated
/// disk below is likewise process-wide, so those tests cannot run in parallel.
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Stand-in for `kernel/src/serial.rs`'s `serial_println!`.
///
/// Arguments are still type-checked and evaluated, so a formatting bug in the
/// code under test still reproduces here; the text is simply discarded unless
/// `TUWAIQFS_TEST_VERBOSE` is set.
#[macro_export]
macro_rules! serial_println {
    ($($arg:tt)*) => {{
        if $crate::verbose() {
            eprintln!($($arg)*);
        }
    }};
}

pub fn verbose() -> bool {
    static VERBOSE: OnceLock<bool> = OnceLock::new();
    *VERBOSE.get_or_init(|| std::env::var_os("TUWAIQFS_TEST_VERBOSE").is_some())
}

// ---------------------------------------------------------------- fake disk

struct Disk {
    sectors: HashMap<u32, [u8; 512]>,
    /// When set, every write at or past this LBA fails. Models the
    /// `Err("ata error")` / `Err("ata busy timeout")` paths in `ata.rs`.
    fail_writes_from: Option<u32>,
    /// Reads at or past this LBA fail, simulating a disk that can still be
    /// written but no longer returns what was written back
    fail_reads_from: Option<u32>,
    writes: usize,
}

fn disk() -> MutexGuard<'static, Disk> {
    static DISK: OnceLock<Mutex<Disk>> = OnceLock::new();
    DISK.get_or_init(|| {
        Mutex::new(Disk {
            sectors: HashMap::new(),
            fail_writes_from: None,
            fail_reads_from: None,
            writes: 0,
        })
    })
    .lock()
    .expect("disk mutex poisoned")
}

/// Replacement for `kernel/src/ata.rs`, with the same signatures the
/// filesystem calls. Unwritten sectors read back as zeroes, matching a
/// freshly formatted region.
pub mod ata {
    use super::disk;

    pub fn read_sector(lba: u32, buffer: &mut [u8; 512]) -> Result<(), &'static str> {
        let d = disk();
        if let Some(fail_from) = d.fail_reads_from {
            if lba >= fail_from {
                return Err("ata error");
            }
        }
        match d.sectors.get(&lba) {
            Some(sector) => buffer.copy_from_slice(sector),
            None => buffer.fill(0),
        }
        Ok(())
    }

    pub fn write_sector(lba: u32, buffer: &[u8; 512]) -> Result<(), &'static str> {
        let mut d = disk();
        if let Some(fail_from) = d.fail_writes_from {
            if lba >= fail_from {
                return Err("ata error");
            }
        }
        d.sectors.insert(lba, *buffer);
        d.writes += 1;
        Ok(())
    }
}

// -------------------------------------------------------- kernel modules

/// `kernel/src/tuwaiqfs.rs`, compiled unmodified.
pub mod tuwaiqfs {
    include!(concat!(env!("OUT_DIR"), "/tuwaiqfs_gen.rs"));

    // Test-only accessors for the private parsing functions. Declared after
    // the include! and therefore inside the same module, so they can see them.
    pub fn deserialize_tree_for_test(data: &[u8]) -> Result<FsNode, &'static str> {
        deserialize_tree(data)
    }
    pub fn serialize_tree_for_test(root: &FsNode) -> Result<Vec<u8>, &'static str> {
        serialize_tree(root)
    }
    pub fn read_metadata_len_for_test(superblock: &[u8; 512]) -> usize {
        read_metadata_len(superblock)
    }
    // The kernel dropped the metadata-length argument from `build_superblock`
    // and now records the length elsewhere. The rig follows rather than
    // preserving the old shape, because the point of this crate is to compile
    // whatever the kernel currently has.
    pub fn build_superblock_for_test() -> [u8; 512] {
        build_superblock()
    }
    // The v2 superblock is still an accepted input on mount but the kernel no
    // longer writes one, so tests have to build it by hand and need the magic.
    pub const MAGIC_FOR_TEST: [u8; 8] = MAGIC;
}

/// `kernel/src/fs.rs`, compiled unmodified.
pub mod fs {
    include!(concat!(env!("OUT_DIR"), "/fs_gen.rs"));
}

// ------------------------------------------------------------- test helpers

/// Wipe the simulated disk and clear any injected write failures.
pub fn reset_disk() {
    let mut d = disk();
    d.sectors.clear();
    d.fail_writes_from = None;
    d.fail_reads_from = None;
    d.writes = 0;
}

pub fn put_sector(lba: u32, sector: [u8; 512]) {
    disk().sectors.insert(lba, sector);
}

pub fn get_sector(lba: u32) -> Option<[u8; 512]> {
    disk().sectors.get(&lba).copied()
}

/// How many distinct sectors have ever been written
///
/// A checkpoint scheme that leaks a sector per sync shows up here as a count
/// that keeps climbing while the tree stays the same size
pub fn sector_count() -> usize {
    disk().sectors.len()
}

/// Make every `write_sector` at or past `lba` fail, simulating a disk that
/// dies partway through a multi-sector metadata flush.
pub fn fail_writes_from(lba: u32) {
    disk().fail_writes_from = Some(lba);
}

pub fn clear_write_failures() {
    disk().fail_writes_from = None;
}

/// Make every `read_sector` at or past `lba` fail
///
/// A disk that accepts writes and then cannot read them back is the failure a
/// checkpointed filesystem has to survive, since it is what the recovery path
/// meets on the next mount
pub fn fail_reads_from(lba: u32) {
    disk().fail_reads_from = Some(lba);
}

pub fn clear_read_failures() {
    disk().fail_reads_from = None;
}

/// Overwrite one byte of an existing sector, leaving everything else intact
///
/// Silent corruption of a single byte is what a checksum exists to catch so it
/// is worth being able to express exactly that rather than only whole sector
/// damage
pub fn corrupt_byte(lba: u32, offset: usize, value: u8) {
    let mut d = disk();
    if let Some(sector) = d.sectors.get_mut(&lba) {
        sector[offset] = value;
    }
}

/// Acquire [`TEST_LOCK`], format a fresh region, and mount an empty
/// filesystem. The returned guard must stay alive for the whole test.
///
/// Do not call [`reset_disk`] afterwards. `tuwaiqfs.rs` keeps its format
/// version, active slot and generation counter in statics that outlive any one
/// test, and this function is what brings them back into agreement with the
/// disk by mounting it. Wiping the disk again leaves the statics believing a
/// superblock is present that is not, and `sync_tree` then skips rewriting it
/// because its recorded format version already matches. The result is a
/// filesystem that syncs successfully and refuses to mount.
#[must_use = "the returned guard keeps the global filesystem locked"]
pub fn with_fresh_fs() -> MutexGuard<'static, ()> {
    // Recover rather than propagate: one failing test should not cascade into
    // every later test reporting a poisoned mutex instead of its own result.
    let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_disk();

    // Leave the disk completely blank rather than pre-writing a superblock.
    //
    // `tuwaiqfs.rs` keeps its format version, active slot and generation in
    // statics that outlive any one test, and the rig cannot reach them. A
    // fully zeroed disk is the one input that makes the kernel reset all three
    // itself, because `mount` treats it as unformatted and runs
    // `format_region`, which stores every one of them before writing a first
    // checkpoint.
    //
    // Placing a v3 superblock here instead looks tidier and is wrong: `mount`
    // then goes looking for checkpoints, finds none, and returns an error
    // before it reaches the stores. The statics keep the previous test's
    // values, and the damage shows up as an unrelated test failing only when
    // the suite is run as a whole.
    fs::init().expect("a blank disk must format and mount cleanly");
    guard
}

/// Unwrap the error of a `Result` whose `Ok` type has no `Debug`.
///
/// `FsNode` deliberately derives nothing in the kernel, so `unwrap_err()` --
/// which requires `T: Debug` -- cannot be used on a parse result. Adding a
/// derive to `kernel/src` just to satisfy the tests would be the tests
/// changing the kernel, which this crate must never do.
#[track_caller]
pub fn expect_err<T>(result: Result<T, &'static str>) -> &'static str {
    match result {
        Ok(_) => panic!("expected an error, got Ok"),
        Err(reason) => reason,
    }
}

/// Build the on-disk metadata blob for a single directory record nested
/// `depth` levels deep (`a/a/a/...`).
///
/// Returns `None` past depth 128, where the path no longer fits the `u8`
/// `path_len` field.
pub fn deep_directory_record(depth: usize) -> Option<Vec<u8>> {
    let mut path = String::new();
    for i in 0..depth {
        if i > 0 {
            path.push('/');
        }
        path.push('a');
    }
    if path.len() > 255 {
        return None;
    }
    let mut blob = Vec::with_capacity(path.len() + 2);
    blob.push(2); // directory record
    blob.push(path.len() as u8);
    blob.extend_from_slice(path.as_bytes());
    Some(blob)
}

/// Deepest chain of nested directories in a tree.
pub fn tree_depth(node: &tuwaiqfs::FsNode) -> usize {
    match node {
        tuwaiqfs::FsNode::File { .. } => 1,
        tuwaiqfs::FsNode::Dir { children } => {
            1 + children
                .iter()
                .map(|(_, child)| tree_depth(child))
                .max()
                .unwrap_or(0)
        }
    }
}

/// Flatten a tree into sorted `path -> content` pairs, so two trees can be
/// compared without depending on child ordering.
pub fn flatten(node: &tuwaiqfs::FsNode) -> Vec<(String, Option<Vec<u8>>)> {
    let mut out = Vec::new();
    walk(node, String::new(), &mut out);
    out.sort();
    out
}

// File contents became `Vec<u8>` in the kernel, so the snapshot type follows.
// Comparing raw bytes is also stricter than comparing strings: a round trip
// that mangles a byte sequence which happens not to be valid UTF-8 is now
// caught rather than being impossible to express.
fn walk(node: &tuwaiqfs::FsNode, path: String, out: &mut Vec<(String, Option<Vec<u8>>)>) {
    match node {
        tuwaiqfs::FsNode::File { content } => out.push((path, Some(content.clone()))),
        tuwaiqfs::FsNode::Dir { children } => {
            if !path.is_empty() {
                out.push((path.clone(), None));
            }
            for (name, child) in children {
                let child_path = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}/{name}")
                };
                walk(child, child_path, out);
            }
        }
    }
}

/// Deterministic xorshift64* PRNG, so any failure replays from its seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    pub fn byte(&mut self) -> u8 {
        (self.next_u64() >> 24) as u8
    }
}
