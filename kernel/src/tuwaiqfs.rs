//! TuwaiqFS v3 - recoverable persistent filesystem on the boot disk.
//!
//! ## Disk layout (512-byte sectors)
//!
//! ```text
//! LBA 0–8191     Bootloader / kernel area (do not modify)
//! LBA 8192       Superblock
//! LBA 8193–8464  Legacy data slots (reserved)
//! LBA 8465–8712  Legacy v2 metadata (migration source)
//! LBA 23490–24001 Checkpoint A (header + payload)
//! LBA 24002–24513 Checkpoint B (header + payload)
//! ```
//!
//! v3 stores the entire directory tree in alternating checksummed checkpoints.
//! The commit marker is written last; mount selects the newest valid committed
//! generation and recovers the older one when the newer copy is incomplete or
//! corrupt. Legacy v2 volumes are upgraded after the first successful write.
//!
//! ## Metadata length tracking (Phase 1 bugfix)
//!
//! Earlier builds inferred where the metadata blob ended by scanning for a
//! zero byte followed by nothing but zero padding. That heuristic silently
//! corrupted every reboot: a serialized record's own `content_len` field is
//! a little-endian `u16`, so any file under 256 bytes produces a zero byte
//! (the length's high byte) immediately followed by real, non-zero content
//! -- indistinguishable, under the old heuristic, from "data ends here, the
//! rest is padding". The scan then fell back to treating the *entire*
//! 512-byte sector as data, the deserializer choked on the trailing zero
//! bytes it misread as more records, and `fs::init()`'s error fallback
//! silently substituted an empty filesystem. Every reboot looked like data
//! loss because, functionally, it was: this is what actually caused
//! `write hello.txt hello` + `reboot` + `cat hello.txt` to fail before this
//! fix, despite being the project's own documented validation example.
//!
//! Version 2 fixed that bug by storing the real blob length explicitly in the
//! superblock. Version 3 stores the exact length and CRC in each checkpoint
//! header, so neither format relies on scanning or end-of-data guessing.

use alloc::string::String;
use alloc::vec::Vec;

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::storage;

pub const SUPERBLOCK_LBA: u32 = 8192;
pub const LEGACY_METADATA_LBA: u32 = 8465;
pub const LEGACY_METADATA_SECTORS: u32 = 248;
pub const SLOT_A_LBA: u32 = 23_490;
pub const SLOT_SECTORS: u32 = 512;
pub const SLOT_B_LBA: u32 = SLOT_A_LBA + SLOT_SECTORS;
pub const MAX_METADATA_BYTES: usize = ((SLOT_SECTORS - 1) * 512) as usize;
/// The v2 record format stores file length as a little-endian `u16`.
/// Keeping the limit at that format boundary preserves compatibility with
/// every existing v2 volume while allowing native ELF files to be stored.
pub const MAX_FILE_SIZE: usize = u16::MAX as usize;
pub const VERSION: u32 = 3;
const LEGACY_VERSION: u32 = 2;
/// Bounds parser-owned node allocations independently of metadata byte size.
/// Existing volumes produced by TuwaiqOS remain far below this limit.
const MAX_NODE_COUNT: usize = 1024;

const MAGIC: [u8; 8] = *b"TQFSv2\0\0";
const TREE_MAGIC: [u8; 4] = *b"TREE";
const SLOT_MAGIC: [u8; 8] = *b"TQCKPT\0\0";
const SLOT_COMMITTED: u32 = 0x434F_4D54;
static ACTIVE_SLOT: AtomicU32 = AtomicU32::new(0);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static FORMAT_VERSION: AtomicU32 = AtomicU32::new(VERSION);

/// Superblock field offsets (all little-endian).
const SB_METADATA_LEN_OFFSET: usize = 20;

/// Serialized node loaded from or written to disk.
pub enum FsNode {
    File { content: Vec<u8> },
    Dir { children: Vec<(String, FsNode)> },
}

/// Load TuwaiqFS from disk, formatting if needed.
pub fn mount() -> Result<FsNode, &'static str> {
    let mut superblock = [0u8; 512];
    storage::read_sector(SUPERBLOCK_LBA, &mut superblock)?;

    if superblock[..8] != MAGIC {
        let mut slot_a = [0u8; 512];
        let mut slot_b = [0u8; 512];
        storage::read_sector(SLOT_A_LBA, &mut slot_a)?;
        storage::read_sector(SLOT_B_LBA, &mut slot_b)?;
        if superblock.iter().any(|byte| *byte != 0)
            || slot_a.iter().any(|byte| *byte != 0)
            || slot_b.iter().any(|byte| *byte != 0)
        {
            return Err("corrupt TuwaiqFS superblock or checkpoint headers");
        }
        crate::serial_println!("tuwaiqfs: unformatted region, creating v3 checkpoints");
        format_region()?;
        return Ok(empty_root());
    }

    let version = u32::from_le_bytes(superblock[8..12].try_into().unwrap());
    let metadata_lba = u32::from_le_bytes(superblock[12..16].try_into().unwrap());
    let metadata_sectors = u32::from_le_bytes(superblock[16..20].try_into().unwrap());
    if version == VERSION {
        if metadata_lba != SLOT_A_LBA || metadata_sectors != SLOT_SECTORS {
            return Err("invalid TuwaiqFS v3 checkpoint geometry");
        }
        FORMAT_VERSION.store(VERSION, Ordering::Release);
        return load_latest_checkpoint();
    }
    if version != LEGACY_VERSION
        || metadata_lba != LEGACY_METADATA_LBA
        || metadata_sectors != LEGACY_METADATA_SECTORS
    {
        return Err("invalid TuwaiqFS superblock geometry or version");
    }

    let metadata_len = read_metadata_len(&superblock);
    if metadata_len > (LEGACY_METADATA_SECTORS * 512) as usize {
        return Err("TuwaiqFS metadata length exceeds reserved region");
    }
    crate::serial_println!(
        "tuwaiqfs: mounting existing tree, {} bytes of metadata",
        metadata_len
    );
    FORMAT_VERSION.store(LEGACY_VERSION, Ordering::Release);
    ACTIVE_SLOT.store(0, Ordering::Release);
    GENERATION.store(0, Ordering::Release);
    load_tree_at(LEGACY_METADATA_LBA, metadata_len)
}

fn read_metadata_len(superblock: &[u8; 512]) -> usize {
    let bytes = &superblock[SB_METADATA_LEN_OFFSET..SB_METADATA_LEN_OFFSET + 4];
    u32::from_le_bytes(bytes.try_into().unwrap()) as usize
}

fn format_region() -> Result<(), &'static str> {
    let superblock = build_superblock();
    storage::write_sector(SUPERBLOCK_LBA, &superblock)?;
    storage::flush()?;
    FORMAT_VERSION.store(VERSION, Ordering::Release);
    ACTIVE_SLOT.store(1, Ordering::Release);
    GENERATION.store(0, Ordering::Release);
    sync_tree(&empty_root())
}

fn build_superblock() -> [u8; 512] {
    let mut superblock = [0u8; 512];
    superblock[..8].copy_from_slice(&MAGIC);
    superblock[8..12].copy_from_slice(&VERSION.to_le_bytes());
    superblock[12..16].copy_from_slice(&SLOT_A_LBA.to_le_bytes());
    superblock[16..20].copy_from_slice(&SLOT_SECTORS.to_le_bytes());
    superblock
}

/// Persist the full filesystem tree to disk.
pub fn sync_tree(root: &FsNode) -> Result<(), &'static str> {
    let blob = serialize_tree(root)?;
    if blob.len() > MAX_METADATA_BYTES {
        return Err("filesystem metadata too large");
    }
    let generation = GENERATION
        .load(Ordering::Acquire)
        .checked_add(1)
        .ok_or("checkpoint generation exhausted")?;
    let target = 1 - ACTIVE_SLOT.load(Ordering::Acquire).min(1);
    write_checkpoint(target, generation, &blob, None)?;
    if FORMAT_VERSION.load(Ordering::Acquire) != VERSION {
        storage::write_sector(SUPERBLOCK_LBA, &build_superblock())?;
        storage::flush()?;
        FORMAT_VERSION.store(VERSION, Ordering::Release);
    }
    ACTIVE_SLOT.store(target, Ordering::Release);
    GENERATION.store(generation, Ordering::Release);
    Ok(())
}

/// Test-only power-loss injection: write an uncommitted inactive checkpoint
/// and stop after `data_sectors` payload sectors. The active generation and
/// in-memory filesystem remain unchanged.
pub fn sync_tree_interrupted(root: &FsNode, data_sectors: usize) -> Result<(), &'static str> {
    let blob = serialize_tree(root)?;
    if blob.len() > MAX_METADATA_BYTES {
        return Err("filesystem metadata too large");
    }
    let generation = GENERATION
        .load(Ordering::Acquire)
        .checked_add(1)
        .ok_or("checkpoint generation exhausted")?;
    let target = 1 - ACTIVE_SLOT.load(Ordering::Acquire).min(1);
    match write_checkpoint(target, generation, &blob, Some(data_sectors)) {
        Err("injected interrupted checkpoint") => Err("injected interrupted checkpoint"),
        Err(reason) => Err(reason),
        Ok(()) => Err("interruption point exceeded checkpoint length"),
    }
}

fn load_tree_at(metadata_lba: u32, metadata_len: usize) -> Result<FsNode, &'static str> {
    if metadata_len == 0 {
        return Ok(empty_root());
    }
    let blob = read_metadata_at(metadata_lba, metadata_len)?;
    if blob.len() < 4 || blob[..4] != TREE_MAGIC {
        return Err("corrupt TuwaiqFS metadata: bad TREE magic");
    }
    deserialize_tree(&blob[4..])
}

fn load_latest_checkpoint() -> Result<FsNode, &'static str> {
    let first = load_checkpoint(0);
    let second = load_checkpoint(1);
    let (slot, (generation, tree)) = match (first, second) {
        (Ok(Some(a)), Ok(Some(b))) => {
            if a.0 >= b.0 {
                (0, a)
            } else {
                (1, b)
            }
        }
        (Ok(Some(a)), _) => {
            crate::serial_println!("tuwaiqfs: recovered from checkpoint A generation {}", a.0);
            (0, a)
        }
        (_, Ok(Some(b))) => {
            crate::serial_println!("tuwaiqfs: recovered from checkpoint B generation {}", b.0);
            (1, b)
        }
        _ => return Err("no valid committed TuwaiqFS checkpoint"),
    };
    ACTIVE_SLOT.store(slot, Ordering::Release);
    GENERATION.store(generation, Ordering::Release);
    crate::serial_println!(
        "tuwaiqfs: mounted checkpoint {} generation {}",
        if slot == 0 { "A" } else { "B" },
        generation
    );
    Ok(tree)
}

fn load_checkpoint(slot: u32) -> Result<Option<(u64, FsNode)>, &'static str> {
    let lba = slot_lba(slot)?;
    let mut header = [0u8; 512];
    storage::read_sector(lba, &mut header)?;
    if header[..8] != SLOT_MAGIC || le_u32(&header[24..28]) != SLOT_COMMITTED {
        return Ok(None);
    }
    let generation = le_u64(&header[8..16]);
    let length = le_u32(&header[16..20]) as usize;
    let expected_crc = le_u32(&header[20..24]);
    if generation == 0 || length == 0 || length > MAX_METADATA_BYTES {
        return Ok(None);
    }
    let blob = read_metadata_at(lba + 1, length)?;
    if crc32(&blob) != expected_crc || blob.len() < 4 || blob[..4] != TREE_MAGIC {
        crate::serial_println!("tuwaiqfs: checkpoint {} checksum invalid", slot);
        return Ok(None);
    }
    let tree = match deserialize_tree(&blob[4..]) {
        Ok(tree) => tree,
        Err(reason) => {
            crate::serial_println!("tuwaiqfs: checkpoint {} parse invalid: {}", slot, reason);
            return Ok(None);
        }
    };
    Ok(Some((generation, tree)))
}

fn write_checkpoint(
    slot: u32,
    generation: u64,
    blob: &[u8],
    interrupt_after: Option<usize>,
) -> Result<(), &'static str> {
    let lba = slot_lba(slot)?;
    let mut header = checkpoint_header(generation, blob, false)?;
    storage::write_sector(lba, &header)?;
    let sectors = blob.len().div_ceil(512);
    let mut sector = [0u8; 512];
    for index in 0..sectors {
        sector.fill(0);
        let offset = index * 512;
        let count = (blob.len() - offset).min(512);
        sector[..count].copy_from_slice(&blob[offset..offset + count]);
        storage::write_sector(lba + 1 + index as u32, &sector)?;
        if interrupt_after == Some(index + 1) {
            storage::flush()?;
            return Err("injected interrupted checkpoint");
        }
    }
    storage::flush()?;
    header = checkpoint_header(generation, blob, true)?;
    storage::write_sector(lba, &header)?;
    storage::flush()
}

fn checkpoint_header(
    generation: u64,
    blob: &[u8],
    committed: bool,
) -> Result<[u8; 512], &'static str> {
    let length = u32::try_from(blob.len()).map_err(|_| "checkpoint length overflow")?;
    let mut header = [0u8; 512];
    header[..8].copy_from_slice(&SLOT_MAGIC);
    header[8..16].copy_from_slice(&generation.to_le_bytes());
    header[16..20].copy_from_slice(&length.to_le_bytes());
    header[20..24].copy_from_slice(&crc32(blob).to_le_bytes());
    if committed {
        header[24..28].copy_from_slice(&SLOT_COMMITTED.to_le_bytes());
    }
    Ok(header)
}

fn slot_lba(slot: u32) -> Result<u32, &'static str> {
    match slot {
        0 => Ok(SLOT_A_LBA),
        1 => Ok(SLOT_B_LBA),
        _ => Err("invalid checkpoint slot"),
    }
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("four-byte field"))
}

fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("eight-byte field"))
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

fn empty_root() -> FsNode {
    FsNode::Dir {
        children: Vec::new(),
    }
}

fn serialize_tree(root: &FsNode) -> Result<Vec<u8>, &'static str> {
    let mut blob = Vec::new();
    blob.extend_from_slice(&TREE_MAGIC);
    flatten_tree("", root, &mut blob)?;
    Ok(blob)
}

fn flatten_tree(path: &str, node: &FsNode, out: &mut Vec<u8>) -> Result<(), &'static str> {
    match node {
        FsNode::File { content } => {
            write_record(out, 1, path, Some(content))?;
        }
        FsNode::Dir { children } => {
            if !path.is_empty() {
                write_record(out, 2, path, None)?;
            }
            for (name, child) in children {
                let child_path = join_path(path, name);
                flatten_tree(&child_path, child, out)?;
            }
        }
    }
    Ok(())
}

fn join_path(base: &str, name: &str) -> String {
    if base.is_empty() {
        String::from(name)
    } else {
        let mut path = String::from(base);
        path.push('/');
        path.push_str(name);
        path
    }
}

fn write_record(
    out: &mut Vec<u8>,
    kind: u8,
    path: &str,
    content: Option<&[u8]>,
) -> Result<(), &'static str> {
    let path_bytes = path.as_bytes();
    if path_bytes.is_empty() || path_bytes.len() > 120 {
        return Err("invalid path");
    }
    out.push(kind);
    out.push(path_bytes.len() as u8);
    out.extend_from_slice(path_bytes);
    if kind == 1 {
        let bytes = content.ok_or("missing file content")?;
        if bytes.len() > MAX_FILE_SIZE {
            return Err("file too large");
        }
        let len = bytes.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(bytes);
    }
    Ok(())
}

fn deserialize_tree(data: &[u8]) -> Result<FsNode, &'static str> {
    let mut root = empty_root();
    let mut offset = 0;
    let mut node_count = 0usize;

    while offset < data.len() {
        node_count = node_count.checked_add(1).ok_or("node count overflow")?;
        if node_count > MAX_NODE_COUNT {
            return Err("too many metadata records");
        }
        if offset + 2 > data.len() {
            return Err("truncated record header");
        }
        let kind = data[offset];
        let path_len = data[offset + 1] as usize;
        offset += 2;

        if offset + path_len > data.len() {
            return Err("truncated record path");
        }
        let path = core::str::from_utf8(&data[offset..offset + path_len])
            .map_err(|_| "invalid path in metadata")?;
        validate_metadata_path(path)?;
        offset += path_len;

        let node = if kind == 1 {
            if offset + 2 > data.len() {
                return Err("truncated file record");
            }
            let content_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
            offset += 2;
            if offset + content_len > data.len() {
                return Err("truncated file content");
            }
            let mut content = Vec::new();
            content
                .try_reserve_exact(content_len)
                .map_err(|_| "file content allocation failed")?;
            content.extend_from_slice(&data[offset..offset + content_len]);
            offset += content_len;
            FsNode::File { content }
        } else if kind == 2 {
            FsNode::Dir {
                children: Vec::new(),
            }
        } else {
            return Err("unknown record kind");
        };

        insert_at_path(&mut root, path, node)?;
    }

    Ok(root)
}

fn insert_at_path(root: &mut FsNode, path: &str, node: FsNode) -> Result<(), &'static str> {
    let mut parts = Vec::new();
    parts
        .try_reserve_exact(path.bytes().filter(|byte| *byte == b'/').count() + 1)
        .map_err(|_| "metadata path allocation failed")?;
    parts.extend(path.split('/').filter(|part| !part.is_empty()));
    if parts.is_empty() {
        return Ok(());
    }

    let mut current = root;
    for (index, part) in parts.iter().enumerate() {
        let is_last = index + 1 == parts.len();
        match current {
            FsNode::Dir { children } => {
                if is_last {
                    if children.iter().any(|(name, _)| name == part) {
                        return Err("duplicate metadata path");
                    }
                    children
                        .try_reserve(1)
                        .map_err(|_| "metadata child allocation failed")?;
                    children.push((try_owned_name(part)?, node));
                    return Ok(());
                }

                // The canonical serializer emits a directory record before
                // every descendant. Requiring that order prevents corrupt
                // paths from synthesizing uncounted implicit nodes.
                let pos = children
                    .iter()
                    .position(|(name, _)| name == part)
                    .ok_or("metadata parent directory missing")?;
                current = &mut children[pos].1;
            }
            FsNode::File { .. } => return Err("path conflict"),
        }
    }
    Ok(())
}

fn try_owned_name(value: &str) -> Result<String, &'static str> {
    let mut owned = String::new();
    owned
        .try_reserve_exact(value.len())
        .map_err(|_| "metadata name allocation failed")?;
    owned.push_str(value);
    Ok(owned)
}

fn validate_metadata_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty()
        || path.len() > 120
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || path.bytes().any(|byte| byte == 0 || byte < 0x20)
    {
        return Err("invalid metadata path");
    }
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." || component.len() > 64 {
            return Err("invalid metadata path component");
        }
    }
    Ok(())
}

/// Read exactly `len` bytes of metadata back from disk. `len` comes from
/// the superblock (see `sync_tree`), so no scanning or end-of-data
/// guessing is needed -- every byte read is known to be real data.
fn read_metadata_at(lba: u32, len: usize) -> Result<Vec<u8>, &'static str> {
    if len > MAX_METADATA_BYTES {
        return Err("metadata length exceeds reserved region");
    }
    let mut blob = Vec::new();
    blob.try_reserve_exact(len)
        .map_err(|_| "metadata buffer allocation failed")?;
    let mut sector_buf = [0u8; 512];

    let mut sector = lba;
    while blob.len() < len {
        storage::read_sector(sector, &mut sector_buf)?;
        let remaining = len - blob.len();
        let take = remaining.min(512);
        blob.extend_from_slice(&sector_buf[..take]);
        sector += 1;
    }

    Ok(blob)
}
