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

    let mut procs = procinfo::list_proc_entries(Duration::from_millis(1));
    procs.sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes));
    let top_consumers: Vec<_> = procs
        .into_iter()
        .take(5)
        .map(|p| json!({ "pid": p.pid, "name": p.name, "bytes": p.memory_bytes }))
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
    let mut procs = procinfo::list_proc_entries(CPU_SAMPLE_WINDOW);
    procs.sort_by(|a, b| b.cpu_percent.partial_cmp(&a.cpu_percent).unwrap_or(std::cmp::Ordering::Equal));

    let processes: Vec<_> = procs
        .into_iter()
        .take(MAX_PROCESSES_RETURNED)
        .map(|p| {
            json!({
                "pid": p.pid,
                "name": p.name,
                "cpu_percent": p.cpu_percent,
                "memory_bytes": p.memory_bytes,
            })
        })
        .collect();

    Ok(json!({ "processes": processes }))
}

pub fn get_network_status(_args: &serde_json::Value) -> ToolResult {
    let interfaces = procinfo::network_status(CPU_SAMPLE_WINDOW);
    let interfaces_json: Vec<_> = interfaces
        .into_iter()
        .map(|iface| {
            json!({
                "name": iface.name,
                "rx_kbps": iface.rx_kbps,
                "tx_kbps": iface.tx_kbps,
            })
        })
        .collect();
    Ok(json!({ "interfaces": interfaces_json }))
}

/// Process names this broker refuses to terminate under any circumstances,
/// regardless of who asked or whether the caller claims confirmation
/// happened -- confirmation is Python/agent.py's job (see `agent.py`'s
/// `SENSITIVE_TOOLS` gate); this list is the broker's own, independent
/// floor, so a compromised or buggy agent still cannot take down the
/// process supervising it or core OS/session infrastructure.
const PROTECTED_PROCESS_NAMES: &[&str] = &[
    "init", "systemd", "kernel", "launchd", "wininit.exe", "csrss.exe",
    "explorer.exe", "tuwaiq-agent-broker",
];

pub fn kill_process(args: &serde_json::Value) -> ToolResult {
    let pid = args
        .get("pid")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            (
                ErrorCode::InvalidArguments,
                "kill_process requires an integer 'pid' argument".to_string(),
            )
        })?;

    // Protect pid 1 unconditionally, by pid rather than by name: pid 1 is
    // *always* the system's init/supervisor process on any Unix-like
    // system regardless of what it happens to be named there (verified
    // during development: in one real environment tested, pid 1 was named
    // neither "init" nor "systemd", and a name-only check would have
    // let this call signal it). Name-matching below is a second,
    // additional layer for well-known non-pid-1 critical processes, not a
    // substitute for this check.
    if pid == 1 {
        return Err((
            ErrorCode::PermissionDenied,
            "pid 1 is always the system init/supervisor process and cannot be terminated".to_string(),
        ));
    }

    let process_name = procinfo::process_name_for_pid(pid as u32).ok_or_else(|| {
        (
            ErrorCode::NotFound,
            format!("no running process with pid {pid}"),
        )
    })?;

    let lower_name = process_name.to_lowercase();
    if PROTECTED_PROCESS_NAMES.iter().any(|p| lower_name == *p) {
        return Err((
            ErrorCode::PermissionDenied,
            format!("'{process_name}' (pid {pid}) is a protected system process and cannot be terminated"),
        ));
    }

    match procinfo::terminate_process(pid as u32) {
        Ok(()) => Ok(json!({ "pid": pid, "name": process_name, "terminated": true })),
        Err(reason) => Err((ErrorCode::InternalError, reason.to_string())),
    }
}

pub fn close_application(args: &serde_json::Value) -> ToolResult {
    let app_id = args
        .get("app_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                ErrorCode::InvalidArguments,
                "close_application requires a string 'app_id' argument".to_string(),
            )
        })?;

    let expected_name = allowlist::expected_process_name(app_id)
        .ok_or_else(|| {
            (
                ErrorCode::NotAllowlisted,
                format!("'{app_id}' is not an allowlisted application"),
            )
        })?
        .to_lowercase();

    let running = procinfo::list_proc_entries(Duration::from_millis(1));
    let target = running
        .into_iter()
        .find(|p| p.name.to_lowercase() == expected_name)
        .ok_or_else(|| {
            (
                ErrorCode::NotFound,
                format!("'{app_id}' does not appear to be running"),
            )
        })?;

    match procinfo::terminate_process(target.pid) {
        Ok(()) => Ok(json!({ "app_id": app_id, "pid": target.pid, "name": target.name, "terminated": true })),
        Err(reason) => Err((ErrorCode::InternalError, reason.to_string())),
    }
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
