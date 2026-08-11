//! Property tests, randomised parsing, and known-issue documentation.
//!
//! ## About the `#[ignore]`d tests
//!
//! Several tests below encode the behaviour TuwaiqFS *should* have. They are
//! marked `#[ignore]` because they do not hold on the current source, so
//! `cargo test` stays green on an unmodified checkout. Each names the issue it
//! belongs to. When that issue is fixed, drop the `#[ignore]` and the test
//! becomes a permanent regression guard.
//!
//! Run them deliberately with:
//!
//! ```text
//! cargo test -- --ignored
//! ```

use tuwaiqfs_host_tests as rig;
use rig::tuwaiqfs::{self, FsNode};
use rig::{fs, Rng};

// ------------------------------------------------- randomised parsing

/// Random bytes must never panic the parser — only ever return `Ok` or `Err`.
#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng::new(0x5EED_0001);
    for _ in 0..200_000 {
        let len = rng.below(512);
        let data: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        let _ = tuwaiqfs::deserialize_tree_for_test(&data);
    }
}

/// Start from well-formed records and corrupt them. Mutating valid structure
/// reaches far deeper into the parser than uniformly random bytes do.
#[test]
fn mutated_records_never_panic() {
    let mut rng = Rng::new(0x5EED_0002);

    for _ in 0..200_000 {
        let mut data = Vec::new();
        for _ in 0..1 + rng.below(8) {
            let kind: u8 = if rng.next_u64() % 3 == 0 { 2 } else { 1 };
            let seg_count = 1 + rng.below(4);
            let mut path = String::new();
            for s in 0..seg_count {
                if s > 0 {
                    path.push('/');
                }
                for _ in 0..1 + rng.below(5) {
                    path.push((b'a' + rng.below(26) as u8) as char);
                }
            }
            data.push(kind);
            data.push(path.len() as u8);
            data.extend_from_slice(path.as_bytes());
            if kind == 1 {
                let content_len = rng.below(32);
                data.extend_from_slice(&(content_len as u16).to_le_bytes());
                for _ in 0..content_len {
                    data.push(b'a' + rng.below(26) as u8);
                }
            }
        }

        // Corrupt it: length fields and UTF-8 lead bytes are where parsers break.
        for _ in 0..1 + rng.below(4) {
            if data.is_empty() {
                break;
            }
            let i = rng.below(data.len());
            match rng.below(4) {
                0 => data[i] = 0xFF,
                1 => data[i] = 0x00,
                2 => data[i] = 0xC0 | (rng.byte() & 0x1F),
                _ => data[i] ^= 1 << rng.below(8),
            }
        }

        let _ = tuwaiqfs::deserialize_tree_for_test(&data);
    }
}

/// Anything the parser accepts, the serializer must be able to write back.
#[test]
fn accepted_trees_can_be_reserialized() {
    let mut rng = Rng::new(0x5EED_0003);
    for _ in 0..50_000 {
        let len = rng.below(256);
        let data: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        if let Ok(tree) = tuwaiqfs::deserialize_tree_for_test(&data) {
            // May legitimately fail (e.g. a path over 120 bytes), but must
            // never panic.
            let _ = tuwaiqfs::serialize_tree_for_test(&tree);
        }
    }
}

// ------------------------------------------------------ nesting depth

/// `path_len` is one byte, so a 255-byte path is the deepest a record can
/// express at all. That was the only bound when this was first written and it
/// left a 128-level tree parseable, which was reported as F6 because the
/// recursive walk needed more stack than a task has.
///
/// The parser now refuses such a path outright, so both limits are asserted
/// here: the encoder still cannot express more than 128 levels, and the parser
/// no longer accepts even that many.
#[test]
fn nesting_depth_is_bounded_by_the_encoder_and_the_parser() {
    assert!(
        rig::deep_directory_record(128).is_some(),
        "128 levels should still fit a 255-byte path"
    );
    assert!(
        rig::deep_directory_record(129).is_none(),
        "129 levels cannot fit the u8 path_len field"
    );

    let blob = rig::deep_directory_record(128).unwrap();
    assert!(
        tuwaiqfs::deserialize_tree_for_test(&blob).is_err(),
        "a 128-level path must be refused rather than walked recursively"
    );
}

/// See SECURITY-AUDIT.md, F6. Recursive tree walks consume roughly 290 bytes
/// per level in the unoptimized profile the kernel ships, so a 128-level tree
/// needs about 36 KiB — more than `task.rs`'s 32 KiB per-task stack, which is
/// a heap `Box` with no guard page.
///
/// Ignored: this documents a proposed limit that the source does not impose.
#[test]
fn deep_nesting_should_be_rejected() {
    let blob = rig::deep_directory_record(128).unwrap();
    assert!(
        tuwaiqfs::deserialize_tree_for_test(&blob).is_err(),
        "a 128-level path should be rejected before it is materialised"
    );
}

// --------------------------------------------- durability / atomicity

/// See SECURITY-AUDIT.md, F3. `sync_tree` writes 248 metadata sectors and only
/// then the superblock that records their length. A disk error in between
/// leaves the superblock advertising a stale length over fresh bytes, so the
/// next mount misparses and `fs::init` silently substitutes an empty
/// filesystem.
///
/// Ignored: documents the desired atomicity, which the source does not provide.
#[test]
fn interrupted_sync_should_not_destroy_the_filesystem() {
    let _guard = rig::with_fresh_fs();

    let good = FsNode::Dir {
        children: vec![(
            "notes.txt".to_string(),
            FsNode::File {
                content: b"important user data".to_vec(),
            },
        )],
    };
    tuwaiqfs::sync_tree(&good).expect("initial sync");

    // A larger tree, with the disk dying three sectors into the flush.
    let bigger = FsNode::Dir {
        children: (0..60)
            .map(|i| {
                (
                    format!("file{i:03}"),
                    FsNode::File {
                        content: vec![b'x'; 120],
                    },
                )
            })
            .collect(),
    };
    rig::fail_writes_from(tuwaiqfs::SLOT_A_LBA + 3);
    let _ = tuwaiqfs::sync_tree(&bigger);
    rig::clear_write_failures();

    let remounted = tuwaiqfs::mount();
    assert!(
        remounted.is_ok(),
        "a failed sync must leave the previous filesystem mountable, got {:?}",
        remounted.err()
    );
    let names = rig::flatten(&remounted.unwrap());
    assert!(
        names.iter().any(|(path, _)| path == "notes.txt"),
        "the previously committed file must survive an interrupted sync"
    );
}


// ----------------------------------------------- fs.rs state machine

/// See SECURITY-AUDIT.md, F7. `fs::touch` pushes the entry before calling
/// `persist()` and does not roll back on failure, so one over-long name
/// poisons the tree and every later write fails.
///
/// Ignored: documents the desired rollback, which the source does not perform.
#[test]
fn failed_touch_should_not_poison_the_filesystem() {
    let _guard = rig::with_fresh_fs();

    fs::create_file_at("/good.txt").expect("a normal name works");
    let _ = fs::create_file_at(&format!("/{}", "a".repeat(121))); // rejected by write_record

    assert!(
        fs::create_file_at("/after.txt").is_ok(),
        "an ordinary name must still work after an earlier name was rejected"
    );
}



/// A write to a path whose parents do not exist used to create the whole
/// chain from unvalidated input with no depth cap, which was reported as F8.
///
/// It now fails instead, so the assertion is inverted: nothing is created and
/// the root is left untouched.
#[test]
fn a_write_does_not_create_its_parent_directories() {
    let _guard = rig::with_fresh_fs();

    let write = fs::write_at("/p/q/r/s/t/u/deep.txt", b"content");
    assert!(
        write.is_err(),
        "writing under directories that do not exist must fail rather than \
         creating a chain of them from unvalidated input"
    );

    let listing = fs::list_at("/").expect("root listing");
    assert!(
        !listing.iter().any(|entry| entry.starts_with('p')),
        "a rejected write must leave nothing behind, got {listing:?}"
    );

    // The same write succeeds once the parents exist through the validated path
    fs::create_dir_at("/p").expect("mkdir p");
    fs::create_dir_at("/p/q").expect("mkdir p/q");
    fs::write_at("/p/q/ok.txt", b"content").expect("write under existing parents");
    assert_eq!(
        fs::read_at("/p/q/ok.txt").expect("read back").as_ref(),
        b"content"
    );
}

/// Round-trip through the whole `fs.rs` layer, not just the serializer.
#[test]
fn fs_layer_survives_a_mount_cycle() {
    let _guard = rig::with_fresh_fs();

    fs::create_dir_at("/docs").expect("mkdir");
    fs::write_at("/readme.txt", b"TuwaiqOS").expect("write");
    fs::write_at("/docs/guide.md", b"# guide").expect("nested write");

    // Re-mount from the same simulated disk.
    fs::init().expect("re-mount");

    assert_eq!(
        fs::read_at("/readme.txt").expect("root file").as_ref(),
        b"TuwaiqOS"
    );
    assert_eq!(
        fs::read_at("/docs/guide.md").expect("nested file").as_ref(),
        b"# guide"
    );
}
