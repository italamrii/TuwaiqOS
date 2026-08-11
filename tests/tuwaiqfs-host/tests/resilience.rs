// Resilience of the TuwaiqFS checkpoint format against damaged metadata and a
// failing disk
//
// These run against `kernel/src/tuwaiqfs.rs` compiled verbatim, so they attack
// the real on-disk format rather than a description of it
//
// The format as written by `write_checkpoint`
//
//   slot LBA        512 byte header
//                     [0..8]   SLOT_MAGIC
//                     [8..16]  generation, u64 little endian
//                     [16..20] payload length, u32 little endian
//                     [20..24] CRC32 of the payload
//                     [24..28] SLOT_COMMITTED, present only once committed
//   slot LBA + 1..  the payload itself
//
// The write order is header uncommitted, then payload, then header committed
// A mount picks the highest generation among slots that are committed and
// whose CRC matches, and falls back to the other slot otherwise
//
// Every test below damages exactly one property of that scheme and asserts the
// filesystem refuses the damage rather than trusting it

use tuwaiqfs_host_tests as rig;
use rig::tuwaiqfs::{self, FsNode};

const HEADER_MAGIC: usize = 0;
const HEADER_GENERATION: usize = 8;
const HEADER_LENGTH: usize = 16;
const HEADER_CRC: usize = 20;
const HEADER_COMMITTED: usize = 24;

fn file(content: &str) -> FsNode {
    FsNode::File {
        content: content.as_bytes().to_vec(),
    }
}

fn dir(children: Vec<(&str, FsNode)>) -> FsNode {
    FsNode::Dir {
        children: children
            .into_iter()
            .map(|(name, node)| (name.to_string(), node))
            .collect(),
    }
}

/// The slot `sync_tree` wrote to, found by which header carries the newer
/// generation
fn active_slot_lba() -> u32 {
    let a = rig::get_sector(tuwaiqfs::SLOT_A_LBA);
    let b = rig::get_sector(tuwaiqfs::SLOT_B_LBA);
    let generation = |sector: Option<[u8; 512]>| -> u64 {
        sector
            .map(|s| u64::from_le_bytes(s[HEADER_GENERATION..HEADER_GENERATION + 8].try_into().unwrap()))
            .unwrap_or(0)
    };
    if generation(a) >= generation(b) {
        tuwaiqfs::SLOT_A_LBA
    } else {
        tuwaiqfs::SLOT_B_LBA
    }
}

fn header_at(lba: u32) -> [u8; 512] {
    rig::get_sector(lba).expect("checkpoint header was written")
}

/// The payload length this slot header records
fn payload_length(lba: u32) -> usize {
    let header = header_at(lba);
    u32::from_le_bytes(header[HEADER_LENGTH..HEADER_LENGTH + 4].try_into().unwrap()) as usize
}

fn put_header(lba: u32, header: [u8; 512]) {
    rig::put_sector(lba, header);
}

/// Assert that whatever `mount` returns, it is not the damaged checkpoint
///
/// A fresh filesystem already carries one valid empty checkpoint, so damaging
/// the slot a later sync wrote leaves a good older copy behind and recovery is
/// entitled to use it. The invariant under test is not that mount fails, which
/// would only hold when there is nothing to fall back to, but that damaged
/// bytes are never handed back as data.
///
/// Both outcomes are acceptable and both are checked: an error, or a tree that
/// does not contain the marker the damaged checkpoint carried.
fn assert_damage_is_not_trusted(marker: &str) {
    match tuwaiqfs::mount() {
        Err(_) => {}
        Ok(tree) => {
            let entries = rig::flatten(&tree);
            assert!(
                !entries.iter().any(|(path, _)| path.contains(marker)),
                "the damaged checkpoint was mounted anyway, '{marker}' came back in {entries:?}"
            );
        }
    }
}

// ----------------------------------------------------------- damaged headers

#[test]
fn a_corrupted_payload_byte_is_caught_by_the_checksum() {
    let _guard = rig::with_fresh_fs();

    let tree = dir(vec![("damaged-crc.txt", file("the original contents"))]);
    tuwaiqfs::sync_tree(&tree).expect("a clean sync works");

    // One byte of the payload, with the header left completely intact and
    // still claiming the checkpoint is committed
    //
    // The offset is taken from the recorded length rather than picked
    // Payload sectors are zero padded to 512 bytes and the CRC covers only the
    // recorded length, so a byte chosen past the end is not corruption at all
    // and the test would pass without the checksum doing anything
    let slot = active_slot_lba();
    let length = payload_length(slot);
    assert!(length > 4, "the payload should be larger than its magic");
    rig::corrupt_byte(slot + 1, length / 2, 0xFF);

    assert_damage_is_not_trusted("damaged-crc.txt");
}

#[test]
fn a_header_claiming_more_than_a_slot_holds_is_refused() {
    let _guard = rig::with_fresh_fs();

    let tree = dir(vec![("damaged-length.txt", file("abc"))]);
    tuwaiqfs::sync_tree(&tree).expect("a clean sync works");

    let slot = active_slot_lba();
    let mut header = header_at(slot);
    header[HEADER_LENGTH..HEADER_LENGTH + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    put_header(slot, header);

    assert_damage_is_not_trusted("damaged-length.txt");
}

#[test]
fn a_length_exactly_one_past_the_slot_capacity_is_refused() {
    let _guard = rig::with_fresh_fs();

    let tree = dir(vec![("damaged-boundary.txt", file("abc"))]);
    tuwaiqfs::sync_tree(&tree).expect("a clean sync works");

    // The boundary rather than an absurd value
    // An off by one in the capacity check is invisible to the u32::MAX case
    let slot = active_slot_lba();
    let mut header = header_at(slot);
    let past = (tuwaiqfs::MAX_METADATA_BYTES + 1) as u32;
    header[HEADER_LENGTH..HEADER_LENGTH + 4].copy_from_slice(&past.to_le_bytes());
    put_header(slot, header);

    assert_damage_is_not_trusted("damaged-boundary.txt");
}

#[test]
fn a_wrong_slot_magic_is_refused() {
    let _guard = rig::with_fresh_fs();

    let tree = dir(vec![("damaged-magic.txt", file("abc"))]);
    tuwaiqfs::sync_tree(&tree).expect("a clean sync works");

    let slot = active_slot_lba();
    let mut header = header_at(slot);
    header[HEADER_MAGIC] ^= 0xFF;
    put_header(slot, header);

    assert_damage_is_not_trusted("damaged-magic.txt");
}

#[test]
fn an_uncommitted_checkpoint_is_not_trusted() {
    let _guard = rig::with_fresh_fs();

    let tree = dir(vec![("uncommitted.txt", file("abc"))]);
    tuwaiqfs::sync_tree(&tree).expect("a clean sync works");

    // Exactly the state a machine that lost power between the payload write
    // and the commit would be left in
    let slot = active_slot_lba();
    let mut header = header_at(slot);
    header[HEADER_COMMITTED..HEADER_COMMITTED + 4].fill(0);
    put_header(slot, header);

    assert_damage_is_not_trusted("uncommitted.txt");
}

// -------------------------------------------------------------- both slots

#[test]
fn both_slots_damaged_is_an_error_and_not_a_panic() {
    let _guard = rig::with_fresh_fs();

    let first = dir(vec![("one.txt", file("first"))]);
    tuwaiqfs::sync_tree(&first).expect("first sync works");
    let second = dir(vec![("two.txt", file("second"))]);
    tuwaiqfs::sync_tree(&second).expect("second sync writes the other slot");

    for slot in [tuwaiqfs::SLOT_A_LBA, tuwaiqfs::SLOT_B_LBA] {
        if let Some(mut header) = rig::get_sector(slot) {
            header[HEADER_CRC..HEADER_CRC + 4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            rig::put_sector(slot, header);
        }
    }

    // The contract here is only that it does not panic and does not return a
    // tree it cannot vouch for
    let result = tuwaiqfs::mount();
    if let Ok(tree) = result {
        assert!(
            rig::flatten(&tree).is_empty(),
            "with both checkpoints unreadable the only defensible result is an \
             empty filesystem, got {:?}",
            rig::flatten(&tree)
        );
    }
}

#[test]
fn a_damaged_newer_checkpoint_falls_back_to_the_older_one() {
    let _guard = rig::with_fresh_fs();

    let first = dir(vec![("keep.txt", file("survives"))]);
    tuwaiqfs::sync_tree(&first).expect("first sync works");
    let first_slot = active_slot_lba();

    let second = dir(vec![("newer.txt", file("lost"))]);
    tuwaiqfs::sync_tree(&second).expect("second sync works");
    let second_slot = active_slot_lba();
    assert_ne!(
        first_slot, second_slot,
        "consecutive syncs must alternate slots or there is nothing to fall back to"
    );

    // Destroy only the newer copy, inside its recorded payload
    let length = payload_length(second_slot);
    rig::corrupt_byte(second_slot + 1, length / 2, 0xFF);

    match tuwaiqfs::mount() {
        Ok(tree) => {
            let entries = rig::flatten(&tree);
            let names: Vec<&String> = entries.iter().map(|(path, _)| path).collect();
            assert!(
                names.iter().any(|p| p.contains("keep.txt")),
                "the older intact checkpoint should have been recovered, got {names:?}"
            );
        }
        Err(error) => panic!(
            "an intact older checkpoint exists so recovery should have used it, got '{error}'"
        ),
    }
}

// ------------------------------------------------------------ failing disk

#[test]
fn a_disk_that_cannot_be_read_is_an_error_and_not_a_panic() {
    let _guard = rig::with_fresh_fs();

    let tree = dir(vec![("f.txt", file("abc"))]);
    tuwaiqfs::sync_tree(&tree).expect("a clean sync works");

    rig::fail_reads_from(0);
    let result = tuwaiqfs::mount();
    rig::clear_read_failures();

    assert!(
        result.is_err(),
        "a disk that returns an error on every read cannot produce a filesystem"
    );
}

#[test]
fn a_write_failure_partway_through_a_checkpoint_leaves_the_previous_one_intact() {
    let _guard = rig::with_fresh_fs();

    let good = dir(vec![("good.txt", file("this must survive"))]);
    tuwaiqfs::sync_tree(&good).expect("the first sync works");
    let good_slot = active_slot_lba();

    // Fail every write into the slot the next sync will target, so the new
    // checkpoint cannot complete
    let doomed_slot = if good_slot == tuwaiqfs::SLOT_A_LBA {
        tuwaiqfs::SLOT_B_LBA
    } else {
        tuwaiqfs::SLOT_A_LBA
    };
    rig::fail_writes_from(doomed_slot);

    let doomed = dir(vec![("doomed.txt", file("this must not appear"))]);
    let sync = tuwaiqfs::sync_tree(&doomed);
    rig::clear_write_failures();

    assert!(sync.is_err(), "a sync onto a failing disk must report failure");

    match tuwaiqfs::mount() {
        Ok(tree) => {
            let entries = rig::flatten(&tree);
            let names: Vec<&String> = entries.iter().map(|(path, _)| path).collect();
            assert!(
                names.iter().any(|p| p.contains("good.txt")),
                "the checkpoint written before the failure must still mount, got {names:?}"
            );
            assert!(
                !names.iter().any(|p| p.contains("doomed.txt")),
                "a checkpoint that never finished writing must not appear, got {names:?}"
            );
        }
        Err(error) => panic!("the earlier good checkpoint should still mount, got '{error}'"),
    }
}

// -------------------------------------------------------- repeated operations

#[test]
fn repeated_syncs_alternate_slots_and_stay_readable() {
    let _guard = rig::with_fresh_fs();

    // Every iteration must be readable and must contain exactly what was last
    // written
    // Alternating slots means an error that only appears on one of them shows
    // up within two iterations rather than never
    let mut previous_slot = None;
    for round in 0..40u32 {
        let name = format!("file{round}.txt");
        let body = format!("contents for round {round}");
        let tree = dir(vec![(name.as_str(), file(&body))]);
        tuwaiqfs::sync_tree(&tree)
            .unwrap_or_else(|error| panic!("sync failed on round {round}: {error}"));

        let slot = active_slot_lba();
        if let Some(previous) = previous_slot {
            assert_ne!(
                previous, slot,
                "round {round} reused the same slot, so a failure here would \
                 destroy the only good copy"
            );
        }
        previous_slot = Some(slot);

        let mounted = tuwaiqfs::mount()
            .unwrap_or_else(|error| panic!("mount failed on round {round}: {error}"));
        let entries = rig::flatten(&mounted);
        assert!(
            entries.iter().any(|(path, content)| path.contains(&name)
                && content.as_deref() == Some(body.as_bytes())),
            "round {round} did not read back what it wrote, got {entries:?}"
        );
    }
}

#[test]
fn repeated_syncs_do_not_grow_the_disk_without_bound() {
    let _guard = rig::with_fresh_fs();

    // The same tree written many times must occupy the same sectors every time
    // A checkpoint scheme that leaked a sector per sync would eventually run
    // past its slot, and counting distinct sectors is the cheapest way to see
    // that happening
    let tree = dir(vec![("stable.txt", file("unchanging"))]);

    tuwaiqfs::sync_tree(&tree).expect("first sync");
    tuwaiqfs::sync_tree(&tree).expect("second sync");
    let after_two = rig::sector_count();

    for _ in 0..30 {
        tuwaiqfs::sync_tree(&tree).expect("repeated sync");
    }
    let after_many = rig::sector_count();

    assert_eq!(
        after_two, after_many,
        "writing the same tree 32 times touched more sectors than writing it \
         twice, which means each checkpoint leaves something behind"
    );
}

#[test]
fn the_generation_counter_advances_on_every_sync() {
    let _guard = rig::with_fresh_fs();

    // Recovery picks the higher generation, so a counter that stalls or goes
    // backwards would silently resurrect an old filesystem
    let tree = dir(vec![("f.txt", file("abc"))]);
    let mut last = 0u64;
    for round in 0..10 {
        tuwaiqfs::sync_tree(&tree).expect("sync works");
        let header = header_at(active_slot_lba());
        let generation =
            u64::from_le_bytes(header[HEADER_GENERATION..HEADER_GENERATION + 8].try_into().unwrap());
        assert!(
            generation > last,
            "round {round} wrote generation {generation} which is not newer than {last}"
        );
        last = generation;
    }
}
