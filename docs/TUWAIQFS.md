# TuwaiqFS v3 - Disk Layout and Recovery

TuwaiqFS is the writable persistent filesystem for TuwaiqOS. It stores the
full directory tree and opaque file data on the boot disk. Version 3 replaces
the v2 single metadata copy with two checksummed, generation-numbered
checkpoints.

## Sector map

All sectors are 512 bytes.

| LBA range | Purpose |
|---|---|
| 0-8191 | Bootloader and kernel; never modified by TuwaiqFS |
| 8192 | Superblock |
| 8193-8464 | Historical reserved area |
| 8465-8712 | Legacy v2 metadata, read only during migration |
| 23490-24001 | Checkpoint A: one header plus 511 payload sectors |
| 24002-24513 | Checkpoint B: one header plus 511 payload sectors |
| 24576 onward | Separate FAT32 resource partition; not TuwaiqFS |

The gap between checkpoint B and the FAT32 partition is deliberate. Geometry
is validated before a volume is accepted.

## Superblock

The v3 superblock remains at LBA 8192 and retains the historical magic so a v2
volume can be recognized and migrated.

| Offset | Size | Field |
|---|---:|---|
| 0 | 8 | Magic: `TQFSv2\0\0` |
| 8 | 4 | Version: `3` |
| 12 | 4 | Checkpoint A LBA: `23490` |
| 16 | 4 | Checkpoint slot size: `512` sectors |

Version 2 superblocks are accepted only with their exact historical geometry.
The first successful mutation writes a valid v3 checkpoint before publishing
the v3 superblock.

## Checkpoint header

Each slot begins with one header sector.

| Offset | Size | Field |
|---|---:|---|
| 0 | 8 | Magic: `TQCKPT\0\0` |
| 8 | 8 | Monotonic generation |
| 16 | 4 | Serialized payload length |
| 20 | 4 | CRC-32 of the exact payload |
| 24 | 4 | Commit marker `0x434F4D54` |

A mutation serializes a candidate tree, writes an uncommitted header to the
inactive slot, writes the complete payload, and writes the committed header
last. Only then does the kernel publish the candidate in memory. An allocation
failure, ATA error, storage-exhaustion rejection, or interrupted payload write
therefore leaves the active generation unchanged.

At mount, both slots are checked independently. TuwaiqFS chooses the highest
valid committed generation. A bad CRC, malformed tree, incomplete header, or
uncommitted newer slot is rejected. If one valid generation remains, mount
reports recovery and uses it. If neither is valid, the root filesystem remains
unavailable in explicit recovery mode; the kernel never substitutes an empty
writable tree or silently reformats a volume that contains checkpoint data.
The separately mounted read-only FAT32 volume at `/boot` remains available for
diagnostics in that state.

## Tree payload

The payload begins with `TREE`, followed by records:

| Field | Size | Description |
|---|---:|---|
| kind | 1 | `1` file, `2` directory |
| path length | 1 | Serialized path length |
| path | variable | Relative normalized path |
| content length | 2 | File size; files only |
| content | variable | Opaque file bytes |

## Limits and validation

- Maximum checkpoint payload: 261,632 bytes.
- Maximum file size: 65,535 bytes, retained from the v2 record format.
- Maximum serialized path: 120 bytes; component: 64 bytes.
- Maximum metadata records: 1,024.
- Nested directories and binary file bodies are supported.

The parser rejects unsupported versions or geometry, oversized lengths,
truncated or unknown records, duplicate paths, missing parent directories,
invalid UTF-8, and invalid components. Parser-owned growth is fallible. It
never clamps a corrupt length or accepts only a valid prefix.

## VFS ownership

TuwaiqFS is mounted read-write at `/` behind the VFS. Normalized paths,
per-process working directories, application-private mutation policy, handles,
and user-pointer validation live above this disk format. No Ring 3 process can
access TuwaiqFS nodes or boot-storage drivers directly. Filesystem allocation,
serialization, and disk I/O run with interrupts enabled and without a global
spin lock held.

## Historical formats

TuwaiqFS v1, formerly AbdullahFS, stored only a flat root. TuwaiqFS v2 added
full-tree persistence and an explicit metadata length at LBA 8465 but used a
single copy. Version 3 retains strict v2 read compatibility solely for safe
migration.
