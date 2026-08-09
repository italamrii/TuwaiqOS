//! Read-only FAT32 backend for the Phase 6 VFS mount table.
//!
//! The build pipeline creates a real third MBR partition containing a small
//! FAT32 resource volume. This reader does not share TuwaiqFS structures or
//! serialization: it validates the MBR/BPB, follows FAT cluster chains, and
//! decodes ordinary 8.3 directory entries directly from disk.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::storage;

const SECTOR_SIZE: usize = 512;
const MBR_PARTITION_OFFSET: usize = 446;
const MBR_PARTITION_COUNT: usize = 4;
const FAT32_MIN_CLUSTERS: u32 = 65_525;
const FAT32_EOC: u32 = 0x0FFF_FFF8;
const FAT32_BAD: u32 = 0x0FFF_FFF7;
const MAX_DIRECTORY_ENTRIES: usize = 4096;
const MAX_READ_FILE_SIZE: usize = 256 * 1024;

#[derive(Clone, Copy)]
pub struct Fat32Volume {
    partition_start: u32,
    partition_sectors: u32,
    sectors_per_cluster: u8,
    fat_start: u32,
    fat_sectors: u32,
    data_start: u32,
    root_cluster: u32,
    cluster_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Clone, Copy)]
pub struct Metadata {
    pub kind: EntryKind,
    pub size: usize,
}

struct DirectoryEntry {
    name: String,
    kind: EntryKind,
    first_cluster: u32,
    size: u32,
}

enum Located {
    Root,
    Entry(DirectoryEntry),
}

impl Fat32Volume {
    pub fn mount() -> Result<Self, &'static str> {
        let mut mbr = [0u8; SECTOR_SIZE];
        storage::read_sector(0, &mut mbr)?;
        if mbr[510] != 0x55 || mbr[511] != 0xAA {
            return Err("FAT32: invalid MBR signature");
        }

        // Prefer the largest FAT32-looking LBA partition. The bootloader's
        // kernel FAT also uses type 0x0C but is FAT16; when multiple 0x0C
        // candidates appear, the packaged resource volume is the large one.
        let mut partition: Option<(u32, u32)> = None;
        for index in 0..MBR_PARTITION_COUNT {
            let offset = MBR_PARTITION_OFFSET + index * 16;
            if matches!(mbr[offset + 4], 0x0B | 0x0C) {
                let start = le_u32(&mbr[offset + 8..offset + 12]);
                let sectors = le_u32(&mbr[offset + 12..offset + 16]);
                if start != 0 && sectors != 0 {
                    let mut candidate = [0u8; SECTOR_SIZE];
                    if storage::read_sector(start, &mut candidate).is_ok()
                        && le_u16(&candidate[17..19]) == 0
                        && le_u16(&candidate[22..24]) == 0
                        && le_u32(&candidate[32..36]) != 0
                    {
                        let replace = match partition {
                            None => true,
                            Some((_, previous_sectors)) => sectors > previous_sectors,
                        };
                        if replace {
                            partition = Some((start, sectors));
                        }
                    }
                }
            }
        }
        let (partition_start, partition_sectors) =
            partition.ok_or("FAT32: MBR partition not found")?;

        let mut bpb = [0u8; SECTOR_SIZE];
        storage::read_sector(partition_start, &mut bpb)?;
        if bpb[510] != 0x55 || bpb[511] != 0xAA || le_u16(&bpb[11..13]) != 512 {
            return Err("FAT32: invalid boot sector");
        }
        let sectors_per_cluster = bpb[13];
        let reserved = u32::from(le_u16(&bpb[14..16]));
        let fat_count = u32::from(bpb[16]);
        let root_entries = le_u16(&bpb[17..19]);
        let total_sectors = le_u32(&bpb[32..36]);
        let fat_sectors = le_u32(&bpb[36..40]);
        let root_cluster = le_u32(&bpb[44..48]) & 0x0FFF_FFFF;
        if sectors_per_cluster == 0
            || !sectors_per_cluster.is_power_of_two()
            || reserved == 0
            || fat_count == 0
            || root_entries != 0
            || total_sectors == 0
            || total_sectors > partition_sectors
            || fat_sectors == 0
            || root_cluster < 2
        {
            return Err("FAT32: invalid BPB geometry");
        }
        let fat_area = fat_count
            .checked_mul(fat_sectors)
            .ok_or("FAT32: FAT geometry overflow")?;
        let data_relative = reserved
            .checked_add(fat_area)
            .ok_or("FAT32: data geometry overflow")?;
        if data_relative >= total_sectors {
            return Err("FAT32: data region missing");
        }
        let data_sectors = total_sectors - data_relative;
        let cluster_count = data_sectors / u32::from(sectors_per_cluster);
        if cluster_count < FAT32_MIN_CLUSTERS {
            return Err("FAT32: volume has non-FAT32 cluster count");
        }
        if u64::from(fat_sectors) * 128 < u64::from(cluster_count) + 2 {
            return Err("FAT32: FAT is too small for data region");
        }
        if root_cluster >= cluster_count + 2 {
            return Err("FAT32: root cluster outside data region");
        }
        let fat_start = partition_start
            .checked_add(reserved)
            .ok_or("FAT32: FAT LBA overflow")?;
        let data_start = partition_start
            .checked_add(data_relative)
            .ok_or("FAT32: data LBA overflow")?;
        partition_start
            .checked_add(total_sectors)
            .ok_or("FAT32: partition LBA overflow")?;

        Ok(Self {
            partition_start,
            partition_sectors: total_sectors,
            sectors_per_cluster,
            fat_start,
            fat_sectors,
            data_start,
            root_cluster,
            cluster_count,
        })
    }

    pub fn kind(&self, path: &str) -> Result<EntryKind, &'static str> {
        match self.locate(path)? {
            Located::Root => Ok(EntryKind::Directory),
            Located::Entry(entry) => Ok(entry.kind),
        }
    }

    pub fn metadata(&self, path: &str) -> Result<Metadata, &'static str> {
        match self.locate(path)? {
            Located::Root => Ok(Metadata {
                kind: EntryKind::Directory,
                size: self.read_directory(self.root_cluster)?.len(),
            }),
            Located::Entry(entry) if entry.kind == EntryKind::File => Ok(Metadata {
                kind: EntryKind::File,
                size: entry.size as usize,
            }),
            Located::Entry(entry) => Ok(Metadata {
                kind: EntryKind::Directory,
                size: self.read_directory(entry.first_cluster)?.len(),
            }),
        }
    }

    pub fn list(&self, path: &str) -> Result<Vec<String>, &'static str> {
        let cluster = match self.locate(path)? {
            Located::Root => self.root_cluster,
            Located::Entry(entry) if entry.kind == EntryKind::Directory => entry.first_cluster,
            Located::Entry(_) => return Err("FAT32: not a directory"),
        };
        let entries = self.read_directory(cluster)?;
        let mut names = Vec::new();
        names
            .try_reserve_exact(entries.len())
            .map_err(|_| "FAT32: directory result allocation failed")?;
        for entry in entries {
            let mut name = entry.name;
            if entry.kind == EntryKind::Directory {
                name.push('/');
            }
            names.push(name);
        }
        Ok(names)
    }

    pub fn read(&self, path: &str) -> Result<Arc<[u8]>, &'static str> {
        let entry = match self.locate(path)? {
            Located::Root => return Err("FAT32: is a directory"),
            Located::Entry(entry) if entry.kind == EntryKind::File => entry,
            Located::Entry(_) => return Err("FAT32: is a directory"),
        };
        let size = entry.size as usize;
        if size > MAX_READ_FILE_SIZE {
            return Err("FAT32: file exceeds read limit");
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|_| "FAT32: file allocation failed")?;
        if size == 0 {
            return Ok(Arc::from(bytes.into_boxed_slice()));
        }
        self.validate_cluster(entry.first_cluster)?;
        let mut cluster = entry.first_cluster;
        let mut visited = 0u32;
        let mut sector = [0u8; SECTOR_SIZE];
        while bytes.len() < size {
            visited = visited
                .checked_add(1)
                .ok_or("FAT32: cluster count overflow")?;
            if visited > self.cluster_count {
                return Err("FAT32: file cluster loop");
            }
            let first_lba = self.cluster_lba(cluster)?;
            for index in 0..u32::from(self.sectors_per_cluster) {
                storage::read_sector(first_lba + index, &mut sector)?;
                let remaining = size - bytes.len();
                bytes.extend_from_slice(&sector[..remaining.min(SECTOR_SIZE)]);
                if bytes.len() == size {
                    break;
                }
            }
            if bytes.len() < size {
                cluster = self
                    .next_cluster(cluster)?
                    .ok_or("FAT32: truncated file chain")?;
            }
        }
        Ok(Arc::from(bytes.into_boxed_slice()))
    }

    fn locate(&self, path: &str) -> Result<Located, &'static str> {
        if !path.starts_with('/') {
            return Err("FAT32: backend path must be absolute");
        }
        let mut cluster = self.root_cluster;
        let mut components = path.split('/').filter(|part| !part.is_empty()).peekable();
        if components.peek().is_none() {
            return Ok(Located::Root);
        }
        while let Some(component) = components.next() {
            let entries = self.read_directory(cluster)?;
            let entry = entries
                .into_iter()
                .find(|entry| entry.name.eq_ignore_ascii_case(component))
                .ok_or("FAT32: entry not found")?;
            if components.peek().is_none() {
                return Ok(Located::Entry(entry));
            }
            if entry.kind != EntryKind::Directory {
                return Err("FAT32: not a directory");
            }
            cluster = entry.first_cluster;
        }
        Err("FAT32: invalid path")
    }

    fn read_directory(&self, start_cluster: u32) -> Result<Vec<DirectoryEntry>, &'static str> {
        self.validate_cluster(start_cluster)?;
        let mut entries = Vec::new();
        let mut cluster = start_cluster;
        let mut visited = 0u32;
        let mut sector = [0u8; SECTOR_SIZE];
        loop {
            visited = visited
                .checked_add(1)
                .ok_or("FAT32: cluster count overflow")?;
            if visited > self.cluster_count {
                return Err("FAT32: directory cluster loop");
            }
            let first_lba = self.cluster_lba(cluster)?;
            for index in 0..u32::from(self.sectors_per_cluster) {
                storage::read_sector(first_lba + index, &mut sector)?;
                for raw in sector.chunks_exact(32) {
                    if raw[0] == 0x00 {
                        return Ok(entries);
                    }
                    if raw[0] == 0xE5 || raw[11] == 0x0F || raw[11] & 0x08 != 0 {
                        continue;
                    }
                    if raw[..11] == *b".          " || raw[..11] == *b"..         " {
                        continue;
                    }
                    if entries.len() >= MAX_DIRECTORY_ENTRIES {
                        return Err("FAT32: directory entry limit exceeded");
                    }
                    entries
                        .try_reserve(1)
                        .map_err(|_| "FAT32: directory allocation failed")?;
                    entries.push(decode_entry(raw)?);
                }
            }
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => return Ok(entries),
            }
        }
    }

    fn next_cluster(&self, cluster: u32) -> Result<Option<u32>, &'static str> {
        self.validate_cluster(cluster)?;
        let byte_offset = cluster.checked_mul(4).ok_or("FAT32: FAT offset overflow")?;
        let sector_offset = byte_offset / SECTOR_SIZE as u32;
        if sector_offset >= self.fat_sectors {
            return Err("FAT32: FAT lookup outside table");
        }
        let mut sector = [0u8; SECTOR_SIZE];
        storage::read_sector(self.fat_start + sector_offset, &mut sector)?;
        let offset = (byte_offset % SECTOR_SIZE as u32) as usize;
        let value = le_u32(&sector[offset..offset + 4]) & 0x0FFF_FFFF;
        if value >= FAT32_EOC {
            Ok(None)
        } else if value == FAT32_BAD || value < 2 {
            Err("FAT32: invalid cluster chain")
        } else {
            self.validate_cluster(value)?;
            Ok(Some(value))
        }
    }

    fn validate_cluster(&self, cluster: u32) -> Result<(), &'static str> {
        if cluster < 2 || cluster >= self.cluster_count + 2 {
            Err("FAT32: cluster outside data region")
        } else {
            Ok(())
        }
    }

    fn cluster_lba(&self, cluster: u32) -> Result<u32, &'static str> {
        self.validate_cluster(cluster)?;
        let relative = (cluster - 2)
            .checked_mul(u32::from(self.sectors_per_cluster))
            .ok_or("FAT32: cluster LBA overflow")?;
        let lba = self
            .data_start
            .checked_add(relative)
            .ok_or("FAT32: data LBA overflow")?;
        let partition_end = self
            .partition_start
            .checked_add(self.partition_sectors)
            .ok_or("FAT32: partition end overflow")?;
        if lba >= partition_end {
            return Err("FAT32: data LBA outside partition");
        }
        Ok(lba)
    }
}

fn decode_entry(raw: &[u8]) -> Result<DirectoryEntry, &'static str> {
    let mut name = String::new();
    name.try_reserve_exact(12)
        .map_err(|_| "FAT32: name allocation failed")?;
    append_short_component(&mut name, &raw[..8])?;
    let extension_len = raw[8..11]
        .iter()
        .rposition(|byte| *byte != b' ')
        .map(|index| index + 1)
        .unwrap_or(0);
    if extension_len != 0 {
        name.push('.');
        append_short_component(&mut name, &raw[8..8 + extension_len])?;
    }
    if name.is_empty() || name == "." || name == ".." {
        return Err("FAT32: invalid short name");
    }
    let high = u32::from(le_u16(&raw[20..22]));
    let low = u32::from(le_u16(&raw[26..28]));
    let first_cluster = ((high << 16) | low) & 0x0FFF_FFFF;
    let kind = if raw[11] & 0x10 != 0 {
        EntryKind::Directory
    } else {
        EntryKind::File
    };
    if kind == EntryKind::Directory && first_cluster < 2 {
        return Err("FAT32: directory has invalid cluster");
    }
    Ok(DirectoryEntry {
        name,
        kind,
        first_cluster,
        size: le_u32(&raw[28..32]),
    })
}

fn append_short_component(out: &mut String, bytes: &[u8]) -> Result<(), &'static str> {
    let length = bytes
        .iter()
        .rposition(|byte| *byte != b' ')
        .map(|index| index + 1)
        .unwrap_or(0);
    for byte in &bytes[..length] {
        if !(0x21..=0x7E).contains(byte) || matches!(*byte, b'/' | b'\\') {
            return Err("FAT32: unsupported short-name byte");
        }
        out.push(*byte as char);
    }
    Ok(())
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
