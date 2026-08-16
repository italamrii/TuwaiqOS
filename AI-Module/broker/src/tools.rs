//! Tool implementations. Each function here is the *only* code path that
//! actually touches the OS for its tool -- Python cannot reach any of this
//! except by sending a well-formed `ToolRequest` for exactly one of these
//! six tool names.

use std::time::Duration;

use serde_json::json;

use crate::allowlist;
use crate::procinfo;
use crate::protocol::ErrorCode;

/// Result of running a tool: either a JSON result payload, or an error code
/// + safe message. Never a raw OS/library error.
pub type ToolResult = Result<serde_json::Value, (ErrorCode, String)>;

const MAX_PROCESSES_RETURNED: usize = 50;
const CPU_SAMPLE_WINDOW: Duration = Duration::from_millis(200);

pub fn get_system_info(_args: &serde_json::Value) -> ToolResult {
    Ok(json!({
        "os_name": "TuwaiqOS",
        "os_version": "v0.5",
        "kernel_version": procinfo::kernel_version(),
        "hostname": procinfo::hostname(),
        "uptime_seconds": procinfo::uptime_seconds(),
    }))
}

pub fn get_cpu_info(_args: &serde_json::Value) -> ToolResult {
    let (aggregate, per_core) = procinfo::cpu_usage_percent(CPU_SAMPLE_WINDOW);
    Ok(json!({
        "model": procinfo::cpu_model_name(),
        "core_count": procinfo::cpu_core_count(),
        "usage_percent": aggregate,
        "per_core_usage_percent": per_core,
    }))
}

pub fn get_memory_info(_args: &serde_json::Value) -> ToolResult {
    let mem = procinfo::read_meminfo();
    let used = mem.total_bytes.saturating_sub(mem.available_bytes);
    let used_percent = if mem.total_bytes > 0 {
        (used as f64 / mem.total_bytes as f64) * 100.0
    } else {
        0.0
    };

    let mut procs = procinfo::list_proc_entries();
    procs.sort_by(|a, b| b.rss_bytes.cmp(&a.rss_bytes));
    let top_consumers: Vec<_> = procs
        .into_iter()
        .take(5)
        .map(|p| json!({ "pid": p.pid, "name": p.name, "bytes": p.rss_bytes }))
        .collect();

    Ok(json!({
        "total_bytes": mem.total_bytes,
        "used_bytes": used,
        "used_percent": used_percent,
        "top_consumers": top_consumers,
    }))
}

pub fn get_disk_info(_args: &serde_json::Value) -> ToolResult {
    let volumes: Vec<_> = procinfo::list_disk_volumes()
        .into_iter()
        .map(|v| {
            let used_percent = if v.total_bytes > 0 {
                (v.used_bytes as f64 / v.total_bytes as f64) * 100.0
            } else {
                0.0
            };
            json!({
                "mount_point": v.mount_point,
                "total_bytes": v.total_bytes,
                "used_bytes": v.used_bytes,
                "used_percent": used_percent,
            })
        })
        .collect();
    Ok(json!({ "volumes": volumes }))
}

pub fn list_processes(_args: &serde_json::Value) -> ToolResult {
    let before = procinfo::list_proc_entries();
    std::thread::sleep(CPU_SAMPLE_WINDOW);
    let after = procinfo::list_proc_entries();

    // Match processes across the two samples by pid to compute a CPU%
    // delta; a process present only in one sample (started or exited mid
    // window) is skipped for this snapshot rather than shown with a
    // meaningless value.
    let mut merged: Vec<_> = after
        .into_iter()
        .filter_map(|a| {
            before.iter().find(|b| b.pid == a.pid).map(|b| {
                let jiffies_delta = a.utime_stime_jiffies.saturating_sub(b.utime_stime_jiffies);
                // USER_HZ is 100 on effectively all modern Linux systems;
                // the alternative (calling sysconf(_SC_CLK_TCK)) adds a
                // libc call for a value that has been 100 in practice for
                // over a decade, so it is treated as a constant here.
                let cpu_percent = (jiffies_delta as f64 * 10.0)
                    / CPU_SAMPLE_WINDOW.as_secs_f64().max(0.001);
                (a, cpu_percent as f32)
            })
        })
        .collect();

    merged.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

    let processes: Vec<_> = merged
        .into_iter()
        .take(MAX_PROCESSES_RETURNED)
        .map(|(p, cpu_percent)| {
            json!({
                "pid": p.pid,
                "name": p.name,
                "cpu_percent": cpu_percent,
                "memory_bytes": p.rss_bytes,
            })
        })
        .collect();

    Ok(json!({ "processes": processes }))
}

pub fn launch_application(args: &serde_json::Value) -> ToolResult {
    let app_id = args
        .get("app_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                ErrorCode::InvalidArguments,
                "launch_application requires a string 'app_id' argument".to_string(),
            )
        })?;

    let entry = allowlist::lookup(app_id).ok_or_else(|| {
        (
            ErrorCode::NotAllowlisted,
            format!("'{app_id}' is not an allowlisted application"),
        )
    })?;

    // Deliberately std::process::Command with a fixed binary path and fixed
    // argument slice from the allowlist entry -- never a shell, never a
    // string built from the request. `app_id` from the request is used only
    // as a lookup key above; it never reaches this call itself.
    match std::process::Command::new(entry.binary_path)
        .args(entry.args)
        .spawn()
    {
        Ok(child) => Ok(json!({
            "app_id": entry.app_id,
            "pid": child.id(),
            "launched": true,
        })),
        Err(e) => Err((
            ErrorCode::InternalError,
            format!("failed to launch '{app_id}': {}", classify_spawn_error(&e)),
        )),
    }
}

/// Reduce a raw `io::Error` to a short, safe category instead of ever
/// forwarding its raw OS message text (which can vary by platform and
/// occasionally embeds path fragments) back over the protocol.
fn classify_spawn_error(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::NotFound => "binary not found on this system",
        std::io::ErrorKind::PermissionDenied => "permission denied",
        _ => "spawn failed",
    }
}
