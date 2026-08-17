//! Minimal Linux system-info reader, direct from `/proc` and `statvfs`.
//!
//! Deliberately does not pull in a large system-info crate: this file is
//! part of the broker's trusted core, so keeping its dependency surface
//! small (just `libc` for `statvfs`/`uname`) makes it easier to fully read
//! and audit end to end -- appropriate for the one process in this system
//! that is allowed to touch the OS.

use std::collections::HashMap;
use std::fs;
use std::time::Duration;

fn read_cpu_lines() -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    if let Ok(content) = fs::read_to_string("/proc/stat") {
        for line in content.lines() {
            if !line.starts_with("cpu") {
                break;
            }
            let fields: Vec<u64> = line
                .split_whitespace()
                .skip(1)
                .filter_map(|f| f.parse::<u64>().ok())
                .collect();
            if fields.len() < 4 {
                continue;
            }
            let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
            let total: u64 = fields.iter().sum();
            out.push((total.saturating_sub(idle), total));
        }
    }
    out
}

/// Percent CPU usage, aggregate and per-core, measured over a short window.
pub fn cpu_usage_percent(sample_window: Duration) -> (f32, Vec<f32>) {
    let before = read_cpu_lines();
    std::thread::sleep(sample_window);
    let after = read_cpu_lines();

    let mut per_core = Vec::new();
    for (b, a) in before.iter().zip(after.iter()) {
        let busy_delta = a.0.saturating_sub(b.0) as f32;
        let total_delta = a.1.saturating_sub(b.1) as f32;
        let pct = if total_delta > 0.0 {
            (busy_delta / total_delta) * 100.0
        } else {
            0.0
        };
        per_core.push(pct);
    }
    let aggregate = per_core.first().copied().unwrap_or(0.0);
    let per_physical_core = if per_core.len() > 1 {
        per_core[1..].to_vec()
    } else {
        Vec::new()
    };
    (aggregate, per_physical_core)
}

pub fn cpu_model_name() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|content| {
            content.lines().find_map(|l| {
                l.strip_prefix("model name")
                    .and_then(|rest| rest.split(':').nth(1))
                    .map(|s| s.trim().to_string())
            })
        })
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn cpu_core_count() -> usize {
    read_cpu_lines().len().saturating_sub(1).max(1)
}

pub struct MemInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

pub fn read_meminfo() -> MemInfo {
    let mut total = 0u64;
    let mut available = 0u64;
    if let Ok(content) = fs::read_to_string("/proc/meminfo") {
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                total = parse_kb_line(rest);
            } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
                available = parse_kb_line(rest);
            }
        }
    }
    MemInfo {
        total_bytes: total,
        available_bytes: available,
    }
}

fn parse_kb_line(rest: &str) -> u64 {
    rest.trim()
        .trim_end_matches(" kB")
        .parse::<u64>()
        .unwrap_or(0)
        .saturating_mul(1024)
}

pub fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

pub fn kernel_version() -> String {
    fs::read_to_string("/proc/version")
        .map(|s| s.split_whitespace().nth(2).unwrap_or("unknown").to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

pub fn uptime_seconds() -> u64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next().map(|s| s.to_string()))
        .and_then(|s| s.parse::<f64>().ok())
        .map(|f| f as u64)
        .unwrap_or(0)
}

pub struct ProcEntry {
    pub pid: u32,
    pub name: String,
    pub rss_bytes: u64,
    pub utime_stime_jiffies: u64,
}

/// Snapshot every readable /proc/<pid>. Processes that disappear mid-read
/// or whose files we lack permission for are silently skipped -- a
/// best-effort system snapshot, not a guaranteed-complete one.
pub fn list_proc_entries() -> Vec<ProcEntry> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(pid_str) = file_name.to_str() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        let base = format!("/proc/{pid}");
        let name = fs::read_to_string(format!("{base}/comm"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        let rss_bytes = fs::read_to_string(format!("{base}/statm"))
            .ok()
            .and_then(|s| s.split_whitespace().nth(1).map(|s| s.to_string()))
            .and_then(|s| s.parse::<u64>().ok())
            .map(|pages| pages.saturating_mul(4096)) // assumes 4K pages
            .unwrap_or(0);
        let utime_stime = fs::read_to_string(format!("{base}/stat"))
            .ok()
            .and_then(|s| parse_stat_jiffies(&s))
            .unwrap_or(0);
        out.push(ProcEntry {
            pid,
            name,
            rss_bytes,
            utime_stime_jiffies: utime_stime,
        });
    }
    out
}

/// `/proc/<pid>/stat` field 14 (utime) and 15 (stime) are jiffies, but the
/// process name field (2) can itself contain spaces/parens, so we must
/// split after the closing `)` rather than by naive whitespace index.
fn parse_stat_jiffies(content: &str) -> Option<u64> {
    let after_name = content.rsplit_once(')')?.1;
    let fields: Vec<&str> = after_name.split_whitespace().collect();
    // fields[0] is state (field 3); utime is field 14 => index 11 here,
    // stime is field 15 => index 12.
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

pub struct DiskVolume {
    pub mount_point: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
}

/// Real mounted filesystems from `/proc/mounts`, filtered to a small
/// allowlist of real block-backed filesystem types -- excludes the usual
/// pseudo-filesystem noise (`proc`, `sysfs`, `cgroup`, `tmpfs`, etc.) so the
/// result is the handful of volumes a user actually cares about, not
/// dozens of virtual mounts.
pub fn list_disk_volumes() -> Vec<DiskVolume> {
    const REAL_FS_TYPES: &[&str] = &["ext4", "ext3", "ext2", "xfs", "btrfs", "vfat", "ntfs", "exfat"];
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut out = Vec::new();

    let Ok(content) = fs::read_to_string("/proc/mounts") else {
        return out;
    };
    for line in content.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        let mount_point = fields[1];
        let fs_type = fields[2];
        if !REAL_FS_TYPES.contains(&fs_type) {
            continue;
        }
        if seen.insert(mount_point.to_string(), ()).is_some() {
            continue;
        }
        if let Some((total, used)) = statvfs_bytes(mount_point) {
            out.push(DiskVolume {
                mount_point: mount_point.to_string(),
                total_bytes: total,
                used_bytes: used,
            });
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn statvfs_bytes(path: &str) -> Option<(u64, u64)> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;

    let c_path = CString::new(path).ok()?;
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    // Safety: `c_path` is a valid, NUL-terminated C string for the
    // duration of this call, and `stat` is a valid, sufficiently sized,
    // writable buffer for `statvfs` to populate. The return value is
    // checked below before the (now-initialized) buffer is read.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // Safety: `rc == 0` guarantees glibc fully populated `stat`.
    let stat = unsafe { stat.assume_init() };
    let block_size = stat.f_frsize as u64;
    let total = stat.f_blocks as u64 * block_size;
    let free = stat.f_bfree as u64 * block_size;
    Some((total, total.saturating_sub(free)))
}

#[cfg(not(target_os = "linux"))]
fn statvfs_bytes(path: &str) -> Option<(u64, u64)> {
    // Broker telemetry is Linux `/proc` + `statvfs`-oriented. Non-Linux
    // hosts can still compile/run policy unit tests; live volume stats are
    // unavailable here.
    let _ = path;
    None
}
