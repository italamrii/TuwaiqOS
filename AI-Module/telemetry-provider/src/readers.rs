//! Cross-platform system readers, built on the `sysinfo` crate.
//!
//! Replaces an earlier Linux-only `/proc`-parsing implementation: this
//! project's actual dev/setup instructions (`ai_development/README.md`)
//! run natively on Windows, so this file must work there too, not just on
//! Linux. `sysinfo` gives one API surface for both, at the cost of one
//! extra dependency versus hand-rolled `/proc` parsing -- an acceptable
//! trade here since correctness on the real target platform matters more
//! than minimizing dependency count for this particular binary (unlike the
//! kernel/broker's own `/proc` reader, this one is not part of a
//! security-sensitive trust boundary).

use std::time::Duration;

use sysinfo::{Networks, System};

/// Two full samples separated by this window, for every rate-based metric
/// (CPU%, disk I/O, network I/O) -- see `main.rs` for why they share one
/// window rather than being sampled independently.
pub const SAMPLE_WINDOW: Duration = Duration::from_millis(600);

pub struct Samples {
    pub cpu_utilization_pct: f64,
    pub ram_utilization_pct: f64,
    pub available_ram_mb: f64,
    pub process_count: u64,
    pub disk_read_kbps: f64,
    pub disk_write_kbps: f64,
    pub network_in_kbps: f64,
    pub network_out_kbps: f64,
    pub uptime_seconds: f64,
}

/// Take one complete, internally-consistent snapshot. All rate metrics
/// (CPU/disk/network) are measured across the *same* `SAMPLE_WINDOW`, using
/// `sysinfo`'s own recommended two-refresh-with-a-wait pattern for a
/// meaningful (non-zero-on-first-read) percentage.
pub fn take_samples() -> Samples {
    let mut sys = System::new_all();
    let mut networks = Networks::new_with_refreshed_list();

    // First refresh establishes a baseline; sysinfo's CPU/network deltas
    // are meaningless (or zero) without a prior sample to diff against.
    sys.refresh_cpu_usage();
    networks.refresh();

    std::thread::sleep(SAMPLE_WINDOW);

    sys.refresh_cpu_usage();
    sys.refresh_memory();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    networks.refresh();

    let cpu_utilization_pct = sys.global_cpu_usage() as f64;

    let total_kb = sys.total_memory();
    let available_kb = sys.available_memory();
    let ram_utilization_pct = if total_kb > 0 {
        ((total_kb.saturating_sub(available_kb)) as f64 / total_kb as f64) * 100.0
    } else {
        0.0
    };
    let available_ram_mb = available_kb as f64 / 1024.0 / 1024.0;

    let process_count = sys.processes().len() as u64;
    let uptime_seconds = System::uptime() as f64;

    let secs = SAMPLE_WINDOW.as_secs_f64().max(0.001);

    // sysinfo's `Disk` type does not expose read/write byte counters in
    // this version -- aggregate I/O instead via `Process::disk_usage()`
    // (delta bytes since each process's last refresh), summed across every
    // process. This is the standard sysinfo pattern for system-wide disk
    // throughput and is implemented cross-platform (Linux `/proc/<pid>/io`,
    // Windows `GetProcessIoCounters`), unlike a per-device counter.
    let (mut read_bytes, mut write_bytes) = (0u64, 0u64);
    for process in sys.processes().values() {
        let usage = process.disk_usage();
        read_bytes += usage.read_bytes;
        write_bytes += usage.written_bytes;
    }
    let disk_read_kbps = (read_bytes as f64 / 1024.0) / secs;
    let disk_write_kbps = (write_bytes as f64 / 1024.0) / secs;

    // Same "since last refresh" delta semantics for network counters.
    // Loopback-style interfaces are excluded by name where recognizable on
    // the current platform, matching the intent (not raw host traffic
    // reflected back at itself) of the schema's network fields.
    let (mut rx_bytes, mut tx_bytes) = (0u64, 0u64);
    for (name, data) in networks.iter() {
        if is_loopback_like(name) {
            continue;
        }
        rx_bytes += data.received();
        tx_bytes += data.transmitted();
    }
    let network_in_kbps = (rx_bytes as f64 / 1024.0) / secs;
    let network_out_kbps = (tx_bytes as f64 / 1024.0) / secs;

    Samples {
        cpu_utilization_pct,
        ram_utilization_pct,
        available_ram_mb,
        process_count,
        disk_read_kbps,
        disk_write_kbps,
        network_in_kbps,
        network_out_kbps,
        uptime_seconds,
    }
}

fn is_loopback_like(interface_name: &str) -> bool {
    let lower = interface_name.to_lowercase();
    lower == "lo" || lower.starts_with("loopback") || lower.contains("loopback")
}

/// Best-effort recent error/warning count. Unix: counts recent
/// `dmesg --level=err,warn` lines (fixed, argument-free invocation --
/// no user input reaches this command). Windows: not yet implemented --
/// the correct equivalent is a Windows Event Log query (System log,
/// Error/Warning levels), which needs the `windows` crate's Event Log
/// APIs; deliberately left as a documented gap (returns 0, meaning "not
/// sampled," not "zero errors observed") rather than shipping something
/// that only *looks* implemented, consistent with
/// `ai_development/system_interface/docs/INTEGRATION.md`'s own convention
/// for metrics without a real source yet.
pub fn read_recent_error_event_count() -> u64 {
    #[cfg(unix)]
    {
        use std::process::Command;
        let output = Command::new("dmesg")
            .arg("--level=err,warn")
            .arg("--since=-2min")
            .output();
        match output {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).lines().count() as u64
            }
            _ => 0,
        }
    }
    #[cfg(not(unix))]
    {
        0
    }
}