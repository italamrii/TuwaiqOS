//! Unit tests for TuwaiqFS serialization (host-side).
//!
//! Listed as a good first issue in CONTRIBUTING.md. Every test here runs
//! against `kernel/src/tuwaiqfs.rs` compiled verbatim — see `src/lib.rs`.

use tuwaiqfs_host_tests as rig;
use rig::tuwaiqfs::{self, FsNode};

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

/// serialize -> deserialize must reproduce the same tree.
fn assert_round_trips(tree: &FsNode) {
    let blob = tuwaiqfs::serialize_tree_for_test(tree).expect("serialize");
    assert_eq!(&blob[..4], b"TREE", "blob must start with the TREE magic");
    let parsed = tuwaiqfs::deserialize_tree_for_test(&blob[4..]).expect("deserialize");
    assert_eq!(
        rig::flatten(tree),
        rig::flatten(&parsed),
        "round-trip changed the tree"
    );
}

#[test]
fn empty_tree_round_trips() {
    assert_round_trips(&dir(vec![]));
}

#[test]
fn single_file_round_trips() {
    assert_round_trips(&dir(vec![("hello.txt", file("hello"))]));
}

#[test]
fn empty_file_round_trips() {
    assert_round_trips(&dir(vec![("empty.txt", file(""))]));
}

#[test]
fn nested_directories_round_trip() {
    assert_round_trips(&dir(vec![
        ("docs", dir(vec![("readme.md", file("# TuwaiqOS"))])),
        ("src", dir(vec![("main.rs", file("fn main() {}"))])),
        ("top.txt", file("at the root")),
    ]));
}

#[test]
fn empty_directory_round_trips() {
    assert_round_trips(&dir(vec![("empty_dir", dir(vec![]))]));
}

#[test]
fn utf8_content_round_trips() {
    assert_round_trips(&dir(vec![
        ("arabic.txt", file("نظام تشغيل طويق")),
        ("mixed.txt", file("TuwaiqOS — نظام 中文 🚀")),
    ]));
}

/// A file's length is stored as a little-endian `u16`, so any content whose
/// length has a zero high byte used to be misread. Guards that regression.
#[test]
fn content_shorter_than_256_bytes_round_trips() {
    for len in [1usize, 2, 15, 100, 254, 255, 256, 257] {
        let content = "x".repeat(len);
        let tree = dir(vec![("f.txt", file(&content))]);
        assert_round_trips(&tree);
    }
}

// ------------------------------------------------------------- boundaries

#[test]
fn path_of_exactly_120_bytes_is_accepted() {
    let name = "a".repeat(120);
    let tree = dir(vec![(name.as_str(), file(""))]);
    assert!(
        tuwaiqfs::serialize_tree_for_test(&tree).is_ok(),
        "120 bytes is the documented maximum and must serialize"
    );
}

#[test]
fn path_longer_than_120_bytes_is_rejected() {
    let name = "a".repeat(121);
    let tree = dir(vec![(name.as_str(), file(""))]);
    assert_eq!(
        tuwaiqfs::serialize_tree_for_test(&tree).unwrap_err(),
        "invalid path"
    );
}

#[test]
fn content_of_exactly_max_file_size_is_accepted() {
    let content = "x".repeat(tuwaiqfs::MAX_FILE_SIZE);
    let tree = dir(vec![("big.txt", file(&content))]);
    assert!(tuwaiqfs::serialize_tree_for_test(&tree).is_ok());
}

#[test]
fn content_past_max_file_size_is_rejected() {
    let content = "x".repeat(tuwaiqfs::MAX_FILE_SIZE + 1);
    let tree = dir(vec![("big.txt", file(&content))]);
    assert_eq!(
        tuwaiqfs::serialize_tree_for_test(&tree).unwrap_err(),
        "file too large"
    );
}

// --------------------------------------------------- malformed input

#[test]
fn truncated_blob_never_panics() {
    let tree = dir(vec![("docs", dir(vec![("a.txt", file("content here"))]))]);
    let blob = tuwaiqfs::serialize_tree_for_test(&tree).unwrap();

    // Every prefix of a valid blob must be handled, not panicked on.
    for cut in 0..blob.len() {
        let _ = tuwaiqfs::deserialize_tree_for_test(&blob[4.min(cut)..cut]);
    }
}

#[test]
fn unknown_record_kind_is_rejected() {
    // kind = 9 is not a file (1) or a directory (2).
    let blob = vec![9u8, 1, b'a'];
    assert_eq!(
        rig::expect_err(tuwaiqfs::deserialize_tree_for_test(&blob)),
        "unknown record kind"
    );
}

#[test]
fn non_utf8_path_is_rejected() {
    let blob = vec![2u8, 2, 0xFF, 0xFE];
    assert_eq!(
        rig::expect_err(tuwaiqfs::deserialize_tree_for_test(&blob)),
        "invalid path in metadata"
    );
}

#[test]
fn content_length_past_end_of_blob_is_rejected() {
    // A file record claiming 0xFFFF bytes of content that is not there.
    let mut blob = vec![1u8, 1, b'f'];
    blob.extend_from_slice(&0xFFFFu16.to_le_bytes());
    assert_eq!(
        rig::expect_err(tuwaiqfs::deserialize_tree_for_test(&blob)),
        "truncated file content"
    );
}

#[test]
fn a_file_cannot_be_used_as_a_directory() {
    // "a" is a file; "a/b" then tries to descend through it.
    let mut blob = vec![1u8, 1, b'a'];
    blob.extend_from_slice(&0u16.to_le_bytes());
    blob.extend_from_slice(&[1u8, 3, b'a', b'/', b'b']);
    blob.extend_from_slice(&0u16.to_le_bytes());
    assert_eq!(
        rig::expect_err(tuwaiqfs::deserialize_tree_for_test(&blob)),
        "path conflict"
    );
}

// --------------------------------------------------------- mount / disk

#[test]
fn mount_formats_an_unwritten_disk() {
    let _guard = rig::with_fresh_fs();

    let root = tuwaiqfs::mount().expect("mount must format rather than fail");
    match root {
        FsNode::Dir { children } => assert!(children.is_empty()),
        FsNode::File { .. } => panic!("root must be a directory"),
    }
}

#[test]
fn sync_then_mount_preserves_the_tree() {
    let _guard = rig::with_fresh_fs();

    let tree = dir(vec![
        ("notes.txt", file("important user data")),
        ("docs", dir(vec![("guide.md", file("how to"))])),
    ]);
    tuwaiqfs::sync_tree(&tree).expect("sync");

    let mounted = tuwaiqfs::mount().expect("mount");
    assert_eq!(rig::flatten(&tree), rig::flatten(&mounted));
}

#[test]
fn a_legacy_superblock_claiming_four_gigabytes_of_metadata_is_refused() {
    let _guard = rig::with_fresh_fs();

    // The v2 layout is still accepted on mount for backwards compatibility, so
    // it is still an input that has to be validated. A v2 superblock is built
    // by hand here because the kernel only ever writes v3 now, which means
    // this path has no other test reaching it.
    let mut superblock = [0u8; 512];
    superblock[..8].copy_from_slice(&tuwaiqfs::MAGIC_FOR_TEST);
    superblock[8..12].copy_from_slice(&2u32.to_le_bytes());
    superblock[12..16].copy_from_slice(&tuwaiqfs::LEGACY_METADATA_LBA.to_le_bytes());
    superblock[16..20].copy_from_slice(&tuwaiqfs::LEGACY_METADATA_SECTORS.to_le_bytes());
    superblock[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    rig::put_sector(tuwaiqfs::SUPERBLOCK_LBA, superblock);

    let result = tuwaiqfs::mount();
    assert!(
        result.is_err(),
        "a claim of 4 GiB in a region of {} bytes must be refused rather than \
         allocated, got {:?}",
        tuwaiqfs::LEGACY_METADATA_SECTORS * 512,
        result.map(|r| rig::flatten(&r))
    );
}

#[test]
fn a_legacy_length_exactly_one_past_the_region_is_refused() {
    let _guard = rig::with_fresh_fs();

    // The boundary rather than an absurd value. An off by one in the bound is
    // invisible to the u32::MAX case above.
    let capacity = tuwaiqfs::LEGACY_METADATA_SECTORS * 512;
    let mut superblock = [0u8; 512];
    superblock[..8].copy_from_slice(&tuwaiqfs::MAGIC_FOR_TEST);
    superblock[8..12].copy_from_slice(&2u32.to_le_bytes());
    superblock[12..16].copy_from_slice(&tuwaiqfs::LEGACY_METADATA_LBA.to_le_bytes());
    superblock[16..20].copy_from_slice(&tuwaiqfs::LEGACY_METADATA_SECTORS.to_le_bytes());
    superblock[20..24].copy_from_slice(&(capacity + 1).to_le_bytes());
    rig::put_sector(tuwaiqfs::SUPERBLOCK_LBA, superblock);

    assert!(
        tuwaiqfs::mount().is_err(),
        "a length one byte past the legacy region must be refused"
    );
}
