//! Cross-platform system reader, built on the `sysinfo` crate.
//!
//! Replaces an earlier Linux-only implementation (direct `/proc` parsing
//! plus `libc::kill`/`libc::statvfs`) that did not compile on Windows at
//! all -- `libc::kill`, `libc::pid_t`, and `libc::statvfs` are POSIX-only
//! and simply do not exist in the `libc` crate's Windows surface. Found
//! directly: this code failed to compile the first time it was built on
//! the project's actual Windows dev machine. `sysinfo` gives one
//! cross-platform API for everything this module needs (CPU, memory,
//! processes, disks, network, and process termination), matching the same
//! fix already applied to `telemetry-provider/src/readers.rs` for the same
//! underlying reason.
//!
//! Function names and signatures are kept identical to the previous
//! `/proc`-based version so `tools.rs` required zero changes.

use std::collections::HashMap;
use std::time::Duration;

use sysinfo::{Disks, Networks, Pid, System};

pub struct MemInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

pub fn read_meminfo() -> MemInfo {
    let mut sys = System::new();
    sys.refresh_memory();
    MemInfo {
        total_bytes: sys.total_memory(),
        available_bytes: sys.available_memory(),
    }
}

/// Aggregate and per-core CPU usage percent, sampled over `sample_window`
/// using `sysinfo`'s recommended two-refresh pattern (a single refresh
/// right after construction has no prior sample to diff against and reads
/// as 0 on most platforms).
pub fn cpu_usage_percent(sample_window: Duration) -> (f32, Vec<f32>) {
    let mut sys = System::new();
    sys.refresh_cpu_usage();
    std::thread::sleep(sample_window);
    sys.refresh_cpu_usage();

    let per_core: Vec<f32> = sys.cpus().iter().map(|c| c.cpu_usage()).collect();
    let aggregate = sys.global_cpu_usage();
    (aggregate, per_core)
}

pub fn cpu_model_name() -> String {
    let sys = System::new_with_specifics(
        sysinfo::RefreshKind::new().with_cpu(sysinfo::CpuRefreshKind::everything()),
    );
    sys.cpus()
        .first()
        .map(|c| c.brand().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn cpu_core_count() -> usize {
    let sys = System::new_with_specifics(
        sysinfo::RefreshKind::new().with_cpu(sysinfo::CpuRefreshKind::everything()),
    );
    sys.cpus().len().max(1)
}

pub fn hostname() -> String {
    System::host_name().unwrap_or_else(|| "unknown".to_string())
}

pub fn kernel_version() -> String {
    System::kernel_version().unwrap_or_else(|| "unknown".to_string())
}

pub fn uptime_seconds() -> u64 {
    System::uptime()
}

pub fn process_name_for_pid(pid: u32) -> Option<String> {
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[Pid::from_u32(pid)]), true);
    sys.process(Pid::from_u32(pid))
        .map(|p| p.name().to_string_lossy().to_string())
}

pub fn find_processes_by_name(name: &str) -> Vec<(u32, String)> {
    let target = normalize_process_name(name);

    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    sys.processes()
        .values()
        .filter_map(|process| {
            let process_name = process.name().to_string_lossy().to_string();

            if normalize_process_name(&process_name) == target {
                Some((process.pid().as_u32(), process_name))
            } else {
                None
            }
        })
        .collect()
}

fn normalize_process_name(name: &str) -> String {
    let name = name.trim().to_lowercase();

    if cfg!(windows) {
        name.strip_suffix(".exe")
            .unwrap_or(&name)
            .to_string()
    } else {
        name
    }
}

/// Request graceful termination. `sysinfo::Process::kill()` sends SIGTERM
/// on Unix and calls `TerminateProcess` on Windows -- the closest
/// cross-platform equivalent to "ask it to stop," abstracting away the
/// platform difference that made the previous `libc::kill`-only
/// implementation Unix-exclusive.
pub fn terminate_process(pid: u32) -> Result<(), &'static str> {
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[Pid::from_u32(pid)]), true);
    match sys.process(Pid::from_u32(pid)) {
        Some(process) => {
            if process.kill() {
                Ok(())
            } else {
                Err("failed to signal process (it may have already exited, or permission was denied)")
            }
        }
        None => Err("process not found"),
    }
}

pub struct ProcEntry {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

/// Snapshot every process, with CPU usage sampled over `window` (same
/// two-refresh pattern as `cpu_usage_percent`).
pub fn list_proc_entries(window: Duration) -> Vec<ProcEntry> {
    let mut sys = System::new_all();
    sys.refresh_cpu_usage();
    std::thread::sleep(window);
    sys.refresh_cpu_usage();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    sys.processes()
        .values()
        .map(|p| ProcEntry {
            pid: p.pid().as_u32(),
            name: p.name().to_string_lossy().to_string(),
            cpu_percent: p.cpu_usage(),
            memory_bytes: p.memory(),
        })
        .collect()
}

pub struct NetworkInterface {
    pub name: String,
    pub rx_kbps: f64,
    pub tx_kbps: f64,
}

/// Per-interface throughput, sampled over `window`. Loopback is excluded.
///
/// Deliberately does NOT report an `is_up`/link-state field: `sysinfo`
/// 0.32 exposes no such method on `NetworkData` at all (confirmed by
/// reading its source). An earlier version of this function inferred "up"
/// from whether the MAC address was all-zero; testing showed known-down
/// virtual interfaces (`ifb0`/`ifb1`) still reported a nonzero MAC and
/// were misreported as up. Rather than hardcode a constant `true` (which
/// would look like real data while being a guess), the field is omitted
/// entirely. A real implementation needs a platform-specific check
/// (`/sys/class/net/<iface>/operstate` on Linux, `GetAdaptersAddresses` on
/// Windows) -- not implemented here; documented as a known gap.
pub fn network_status(window: Duration) -> Vec<NetworkInterface> {
    let mut networks = Networks::new_with_refreshed_list();
    let before = snapshot_network_totals(&networks);
    std::thread::sleep(window);
    networks.refresh();
    let after = snapshot_network_totals(&networks);
    let secs = window.as_secs_f64().max(0.001);

    let mut interfaces = Vec::new();
    for (name, data) in networks.iter() {
        if is_loopback_like(name) {
            continue;
        }
        let (rx_before, tx_before) = before.get(name).copied().unwrap_or((0, 0));
        let (rx_after, tx_after) = after.get(name).copied().unwrap_or((data.total_received(), data.total_transmitted()));
        let rx_kbps = (rx_after.saturating_sub(rx_before) as f64 / 1024.0) / secs;
        let tx_kbps = (tx_after.saturating_sub(tx_before) as f64 / 1024.0) / secs;
        interfaces.push(NetworkInterface {
            name: name.clone(),
            rx_kbps,
            tx_kbps,
        });
    }
    interfaces.sort_by(|a, b| a.name.cmp(&b.name));
    interfaces
}

fn snapshot_network_totals(networks: &Networks) -> HashMap<String, (u64, u64)> {
    networks
        .iter()
        .map(|(name, data)| (name.clone(), (data.total_received(), data.total_transmitted())))
        .collect()
}

fn is_loopback_like(interface_name: &str) -> bool {
    let lower = interface_name.to_lowercase();
    lower == "lo" || lower.starts_with("loopback") || lower.contains("loopback")
}

pub struct DiskVolume {
    pub mount_point: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
}

pub fn list_disk_volumes() -> Vec<DiskVolume> {
    let disks = Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .map(|d| {
            let total = d.total_space();
            let available = d.available_space();
            DiskVolume {
                mount_point: d.mount_point().to_string_lossy().to_string(),
                total_bytes: total,
                used_bytes: total.saturating_sub(available),
            }
        })
        .collect()
}
