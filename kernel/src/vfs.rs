//! Phase 6 virtual filesystem.
//!
//! A longest-prefix mount table dispatches normalized paths to independent
//! backends. TuwaiqFS is writable at `/`; a separately formatted FAT32 volume
//! is mounted read-only at `/boot`. Callers never receive backend nodes.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use spin::Mutex;
use x86_64::instructions::interrupts;

use crate::{fat32, fs};

pub const PATH_MAX: usize = 120;
pub const NAME_MAX: usize = 64;
pub const MAX_EXECUTABLE_SIZE: usize = crate::tuwaiqfs::MAX_FILE_SIZE;
const MAX_MOUNTS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeMetadata {
    pub kind: NodeKind,
    /// File length in bytes, or immediate child count for a directory.
    pub size: usize,
}

#[derive(Clone)]
pub struct MountInfo {
    pub path: String,
    pub label: &'static str,
    pub read_only: bool,
}

enum Backend {
    Tuwaiq,
    Fat32(fat32::Fat32Volume),
}

struct Mount {
    path: String,
    label: &'static str,
    read_only: bool,
    backend: Backend,
}

struct MountTable {
    mounts: Vec<Mount>,
}

static MOUNTS: Mutex<Option<Arc<MountTable>>> = Mutex::new(None);
static SHELL_CWD: Mutex<Option<String>> = Mutex::new(None);

impl MountTable {
    fn new() -> Result<Self, &'static str> {
        let mut mounts = Vec::new();
        mounts
            .try_reserve_exact(MAX_MOUNTS)
            .map_err(|_| "mount table allocation failed")?;
        Ok(Self { mounts })
    }

    fn register(
        &mut self,
        path: &str,
        label: &'static str,
        read_only: bool,
        backend: Backend,
    ) -> Result<(), &'static str> {
        if self.mounts.len() >= MAX_MOUNTS {
            return Err("mount table full");
        }
        let normalized = normalize("/", path)?;
        if normalized != path || self.mounts.iter().any(|mount| mount.path == path) {
            return Err("invalid or duplicate mount path");
        }
        self.mounts.push(Mount {
            path: normalized,
            label,
            read_only,
            backend,
        });
        Ok(())
    }

    fn resolve<'a>(&'a self, absolute: &str) -> Result<(&'a Mount, String), &'static str> {
        let mount = self
            .mounts
            .iter()
            .filter(|mount| mount_matches(&mount.path, absolute))
            .max_by_key(|mount| mount.path.len())
            .ok_or("no VFS mount for path")?;
        let suffix = if mount.path == "/" {
            absolute
        } else {
            absolute
                .strip_prefix(&mount.path)
                .ok_or("mount prefix mismatch")?
        };
        let backend_path = if suffix.is_empty() { "/" } else { suffix };
        let mut owned = String::new();
        owned
            .try_reserve_exact(backend_path.len())
            .map_err(|_| "backend path allocation failed")?;
        owned.push_str(backend_path);
        Ok((mount, owned))
    }
}

fn mount_matches(mount: &str, absolute: &str) -> bool {
    mount == "/"
        || absolute == mount
        || absolute
            .strip_prefix(mount)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn mount_snapshot() -> Result<Arc<MountTable>, &'static str> {
    interrupts::without_interrupts(|| {
        MOUNTS
            .lock()
            .as_ref()
            .map(Arc::clone)
            .ok_or("VFS not initialized")
    })
}

/// Return whether two already-normalized absolute paths resolve through the
/// same longest-prefix mount. Capability scopes use this before authorizing a
/// child path so a scope can never cross from one backend into another.
pub fn same_mount(left: &str, right: &str) -> Result<bool, &'static str> {
    let left = normalize("/", left)?;
    let right = normalize("/", right)?;
    let table = mount_snapshot()?;
    let (left_mount, _) = table.resolve(&left)?;
    let (right_mount, _) = table.resolve(&right)?;
    Ok(left_mount.path == right_mount.path)
}

/// Syscalls enter through an interrupt gate with IF clear. VFS traversal can
/// allocate and FAT32 access performs ATA I/O, so execute backend work with
/// interrupts enabled and restore the caller's original IF state afterward.
fn with_runtime_interrupts<R>(f: impl FnOnce() -> R) -> R {
    let restore_disabled = !interrupts::are_enabled();
    if restore_disabled {
        interrupts::enable();
    }
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            if self.0 {
                interrupts::disable();
            }
        }
    }
    let _restore = Restore(restore_disabled);
    f()
}

pub fn init() {
    let mut table = MountTable::new().expect("VFS mount table allocation failed");
    match fs::init() {
        Ok(()) => table
            .register("/", fs::label(), false, Backend::Tuwaiq)
            .expect("VFS root mount registration failed"),
        Err(reason) => crate::serial_println!(
            "vfs: TuwaiqFS unavailable; entering read-only recovery mode: {}",
            reason
        ),
    }
    match fat32::Fat32Volume::mount() {
        Ok(volume) => table
            .register("/boot", "FAT32", true, Backend::Fat32(volume))
            .expect("VFS FAT32 mount registration failed"),
        Err(reason) => crate::serial_println!("vfs: FAT32 /boot mount failed: {}", reason),
    }
    let table = Arc::new(table);
    let shell_root = String::from("/");
    interrupts::without_interrupts(|| {
        *MOUNTS.lock() = Some(table);
        *SHELL_CWD.lock() = Some(shell_root);
    });
    if kind("/", "/") == Ok(NodeKind::Directory) {
        crate::serial_println!("vfs: mounted {} at /", fs::label());
    }
    if kind("/", "/boot") == Ok(NodeKind::Directory) {
        crate::serial_println!("vfs: mounted FAT32 read-only at /boot");
    }
}

/// Resolve `input` against normalized absolute `cwd`.
pub fn normalize(cwd: &str, input: &str) -> Result<String, &'static str> {
    if input.is_empty() {
        return Err("path required");
    }
    if !cwd.starts_with('/') || cwd.len() > PATH_MAX {
        return Err("invalid working directory");
    }
    if input.contains('\\') || input.bytes().any(|byte| byte == 0 || byte < 0x20) {
        return Err("invalid path character");
    }

    let mut normalized = String::new();
    normalized
        .try_reserve_exact(PATH_MAX)
        .map_err(|_| "path allocation failed")?;
    if input.starts_with('/') {
        normalized.push('/');
    } else {
        if cwd.contains('\\')
            || cwd.bytes().any(|byte| byte == 0 || byte < 0x20)
            || cwd.ends_with('/') && cwd != "/"
        {
            return Err("invalid working directory");
        }
        for component in cwd.split('/').filter(|part| !part.is_empty()) {
            validate_component(component)?;
        }
        normalized.push_str(cwd);
    }

    for component in input.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if normalized.len() > 1 {
                    let separator = normalized.rfind('/').unwrap_or(0);
                    normalized.truncate(separator.max(1));
                }
            }
            value => {
                validate_component(value)?;
                let separator_len = usize::from(normalized != "/");
                let new_len = normalized
                    .len()
                    .checked_add(separator_len)
                    .and_then(|length| length.checked_add(value.len()))
                    .ok_or("path length overflow")?;
                if new_len > PATH_MAX {
                    return Err("path too long");
                }
                if separator_len != 0 {
                    normalized.push('/');
                }
                normalized.push_str(value);
            }
        }
    }
    Ok(normalized)
}

fn validate_component(component: &str) -> Result<(), &'static str> {
    if component.is_empty() || component.len() > NAME_MAX {
        return Err("invalid path component length");
    }
    if component == "." || component == ".." {
        return Err("reserved path component");
    }
    Ok(())
}

fn backend_kind(backend: &Backend, path: &str) -> Result<NodeKind, &'static str> {
    match backend {
        Backend::Tuwaiq => match fs::kind_at(path)? {
            fs::EntryKind::File => Ok(NodeKind::File),
            fs::EntryKind::Directory => Ok(NodeKind::Directory),
        },
        Backend::Fat32(volume) => match volume.kind(path)? {
            fat32::EntryKind::File => Ok(NodeKind::File),
            fat32::EntryKind::Directory => Ok(NodeKind::Directory),
        },
    }
}

pub fn kind(cwd: &str, path: &str) -> Result<NodeKind, &'static str> {
    let absolute = normalize(cwd, path)?;
    with_runtime_interrupts(|| {
        let table = mount_snapshot()?;
        let (mount, backend_path) = table.resolve(&absolute)?;
        backend_kind(&mount.backend, &backend_path)
    })
}

pub fn read_file(cwd: &str, path: &str) -> Result<Arc<[u8]>, &'static str> {
    let absolute = normalize(cwd, path)?;
    with_runtime_interrupts(|| {
        let table = mount_snapshot()?;
        let (mount, backend_path) = table.resolve(&absolute)?;
        match &mount.backend {
            Backend::Tuwaiq => fs::read_at(&backend_path),
            Backend::Fat32(volume) => volume.read(&backend_path),
        }
    })
}

pub fn list_dir(cwd: &str, path: &str) -> Result<Vec<String>, &'static str> {
    let absolute = normalize(cwd, path)?;
    with_runtime_interrupts(|| {
        let table = mount_snapshot()?;
        let (mount, backend_path) = table.resolve(&absolute)?;
        let mut entries = match &mount.backend {
            Backend::Tuwaiq => fs::list_at(&backend_path)?,
            Backend::Fat32(volume) => volume.list(&backend_path)?,
        };
        for child in table.mounts.iter().filter_map(|candidate| {
            if candidate.path == "/" {
                return None;
            }
            let separator = candidate.path.rfind('/')?;
            let parent = if separator == 0 {
                "/"
            } else {
                &candidate.path[..separator]
            };
            if parent == absolute {
                Some(&candidate.path[separator + 1..])
            } else {
                None
            }
        }) {
            if !entries
                .iter()
                .any(|entry| entry.trim_end_matches('/') == child)
            {
                let mut display = String::from(child);
                display.push('/');
                entries.push(display);
            }
        }
        Ok(entries)
    })
}

pub fn metadata(cwd: &str, path: &str) -> Result<NodeMetadata, &'static str> {
    let absolute = normalize(cwd, path)?;
    with_runtime_interrupts(|| {
        let table = mount_snapshot()?;
        let (mount, backend_path) = table.resolve(&absolute)?;
        match &mount.backend {
            Backend::Tuwaiq => {
                let value = fs::metadata_at(&backend_path)?;
                Ok(NodeMetadata {
                    kind: match value.kind {
                        fs::EntryKind::File => NodeKind::File,
                        fs::EntryKind::Directory => NodeKind::Directory,
                    },
                    size: value.size,
                })
            }
            Backend::Fat32(volume) => {
                let value = volume.metadata(&backend_path)?;
                Ok(NodeMetadata {
                    kind: match value.kind {
                        fat32::EntryKind::File => NodeKind::File,
                        fat32::EntryKind::Directory => NodeKind::Directory,
                    },
                    size: value.size,
                })
            }
        }
    })
}

fn mutate(
    cwd: &str,
    path: &str,
    f: impl FnOnce(&str) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    let absolute = normalize(cwd, path)?;
    with_runtime_interrupts(|| {
        let table = mount_snapshot()?;
        let (mount, backend_path) = table.resolve(&absolute)?;
        if mount.read_only || !matches!(mount.backend, Backend::Tuwaiq) {
            return Err("read-only filesystem");
        }
        f(&backend_path)
    })
}

pub fn create_file(cwd: &str, path: &str) -> Result<(), &'static str> {
    mutate(cwd, path, fs::create_file_at)
}

pub fn create_dir(cwd: &str, path: &str) -> Result<(), &'static str> {
    mutate(cwd, path, fs::create_dir_at)
}

pub fn write_file(cwd: &str, path: &str, bytes: &[u8]) -> Result<(), &'static str> {
    mutate(cwd, path, |backend_path| fs::write_at(backend_path, bytes))
}

pub fn remove(cwd: &str, path: &str) -> Result<(), &'static str> {
    mutate(cwd, path, fs::remove_at)
}

pub fn sync() -> Result<(), &'static str> {
    with_runtime_interrupts(fs::sync_to_disk)
}

/// Acceptance-only power-loss injection for the writable TuwaiqFS backend.
/// Normalizes and resolves the path exactly like a write, then asks the
/// backend to stop before its inactive checkpoint can be committed.
pub fn inject_interrupted_write(
    cwd: &str,
    path: &str,
    bytes: &[u8],
    data_sectors: usize,
) -> Result<(), &'static str> {
    let normalized = normalize(cwd, path)?;
    with_runtime_interrupts(|| {
        let table = mount_snapshot()?;
        let (mount, backend_path) = table.resolve(&normalized)?;
        if mount.read_only || !matches!(mount.backend, Backend::Tuwaiq) {
            return Err("interrupted-write injection requires writable TuwaiqFS");
        }
        fs::inject_interrupted_write(&backend_path, bytes, data_sectors)
    })
}

pub fn mounts() -> Result<Vec<MountInfo>, &'static str> {
    let table = mount_snapshot()?;
    let mut result = Vec::new();
    result
        .try_reserve_exact(table.mounts.len())
        .map_err(|_| "mount list allocation failed")?;
    for mount in &table.mounts {
        result.push(MountInfo {
            path: mount.path.clone(),
            label: mount.label,
            read_only: mount.read_only,
        });
    }
    Ok(result)
}

fn shell_cwd() -> Result<String, &'static str> {
    let (bytes, len) = interrupts::without_interrupts(|| {
        let guard = SHELL_CWD.lock();
        let cwd = guard.as_ref().ok_or("VFS not initialized")?;
        if cwd.len() > PATH_MAX {
            return Err("invalid shell working directory");
        }
        let mut bytes = [0u8; PATH_MAX];
        bytes[..cwd.len()].copy_from_slice(cwd.as_bytes());
        Ok((bytes, cwd.len()))
    })?;
    let text =
        core::str::from_utf8(&bytes[..len]).map_err(|_| "invalid shell working directory")?;
    let mut cwd = String::new();
    cwd.try_reserve_exact(len)
        .map_err(|_| "shell working directory allocation failed")?;
    cwd.push_str(text);
    Ok(cwd)
}

pub fn shell_pwd() -> Result<String, &'static str> {
    shell_cwd()
}

pub fn shell_chdir(path: &str) -> Result<String, &'static str> {
    let current = shell_cwd()?;
    let target = normalize(&current, path.trim())?;
    if kind("/", &target)? != NodeKind::Directory {
        return Err("not a directory");
    }
    let result = target.clone();
    interrupts::without_interrupts(|| {
        *SHELL_CWD.lock() = Some(target);
    });
    Ok(result)
}

pub fn shell_list(path: Option<&str>) -> Result<Vec<String>, &'static str> {
    let cwd = shell_cwd()?;
    list_dir(&cwd, path.unwrap_or("."))
}

pub fn shell_read(path: &str) -> Result<String, &'static str> {
    let cwd = shell_cwd()?;
    let bytes = read_file(&cwd, path.trim())?;
    let text = core::str::from_utf8(&bytes).map_err(|_| "file is not UTF-8 text")?;
    Ok(text.to_string())
}

pub fn shell_create_file(path: &str) -> Result<(), &'static str> {
    let cwd = shell_cwd()?;
    create_file(&cwd, path.trim())
}

pub fn shell_create_dir(path: &str) -> Result<(), &'static str> {
    let cwd = shell_cwd()?;
    create_dir(&cwd, path.trim())
}

pub fn shell_write(path: &str, text: &str) -> Result<(), &'static str> {
    let cwd = shell_cwd()?;
    write_file(&cwd, path.trim(), text.as_bytes())
}

pub fn completion_candidates(token: &str) -> Result<Vec<String>, &'static str> {
    let cwd = shell_cwd()?;
    let (typed_parent, leaf) = match token.rfind('/') {
        Some(index) => (&token[..=index], &token[index + 1..]),
        None => ("", token),
    };
    let parent_query = if typed_parent.is_empty() {
        "."
    } else {
        typed_parent
    };
    let parent = normalize(&cwd, parent_query)?;
    let entries = list_dir("/", &parent)?;
    let mut matches = Vec::new();
    for entry in entries {
        if entry.trim_end_matches('/').starts_with(leaf) {
            let mut candidate = String::from(typed_parent);
            candidate.push_str(&entry);
            matches.push(candidate);
        }
    }
    Ok(matches)
}

pub fn basename(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("process")
}

pub fn self_test() -> Result<(), &'static str> {
    let cases = [
        (("/", "/"), "/"),
        (("/home/user", "../bin/./tool"), "/home/bin/tool"),
        (("/home/user", "../../../../etc"), "/etc"),
        (("/", "//apps///hello"), "/apps/hello"),
        (("/a", "."), "/a"),
    ];
    for ((cwd, input), expected) in cases {
        if normalize(cwd, input)? != expected {
            return Err("path normalization mismatch");
        }
    }
    if normalize("/", "").is_ok()
        || normalize("/", "bad\\path").is_ok()
        || normalize("/", "bad\0path").is_ok()
        || normalize("/", &"x".repeat(NAME_MAX + 1)).is_ok()
    {
        return Err("invalid path accepted");
    }
    if kind("/", "/boot")? != NodeKind::Directory
        || read_file("/", "/boot/README.TXT")?.as_ref() != b"TuwaiqOS FAT32 resource volume\n"
        || read_file("/", "/boot/DOCS/APPS.TXT")?.as_ref() != b"desktop\nfile-manager\nterminal\n"
        || write_file("/", "/boot/REJECT.TXT", b"no").is_ok()
    {
        return Err("FAT32 mount/backend contract mismatch");
    }
    Ok(())
}
