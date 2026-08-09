//! System monitor — RAM, tasks, filesystem, network snapshot.

use alloc::string::String;
use alloc::vec::Vec;

use bootloader_api::{info::MemoryRegionKind, BootInfo};

use crate::allocator;
use crate::memory;
use crate::net;
use crate::paging;
use crate::task;
use crate::vfs;

/// Collect monitor output lines.
pub fn snapshot(boot_info: &BootInfo) -> Result<Vec<String>, &'static str> {
    let mut lines = Vec::new();
    lines.push(String::from("TuwaiqOS System Monitor"));
    lines.push(String::from(""));

    lines.push(String::from("[ RAM ]"));
    let mut usable = 0u64;
    for region in boot_info.memory_regions.iter() {
        if region.kind == MemoryRegionKind::Usable {
            usable = usable.saturating_add(region.end.saturating_sub(region.start));
        }
    }
    lines.push(format_u64_line("  Usable RAM: ", usable, " bytes"));
    lines.push(format_u64_line(
        "  Kernel heap: ",
        memory::HEAP_SIZE as u64,
        " bytes",
    ));
    lines.push(format_u64_line(
        "    used: ",
        allocator::used() as u64,
        " bytes",
    ));
    lines.push(format_u64_line(
        "    free: ",
        allocator::free() as u64,
        " bytes",
    ));
    if paging::is_active() {
        lines.push(String::from("  Paging: active (heap is real mapped pages)"));
        if let Some(stats) = paging::frame_stats() {
            lines.push(format_u64_line(
                "    physical frames allocated: ",
                stats.allocated as u64,
                "",
            ));
            lines.push(format_u64_line(
                "    physical frames in free pool: ",
                stats.free_in_pool as u64,
                "",
            ));
        }
    } else {
        lines.push(String::from(
            "  Paging: inactive (heap fell back to a static array)",
        ));
    }
    lines.push(String::from(""));

    lines.push(String::from("[ Tasks ]"));
    for t in task::list()? {
        let mut line = String::from("  PID ");
        line.push_str(&u32_to_string(t.id));
        line.push_str("  ");
        line.push_str(&t.name);
        line.push_str("  (");
        line.push_str(task::state_label(t.state));
        line.push(')');
        lines.push(line);
    }
    lines.push(String::from(""));

    lines.push(String::from("[ Filesystem ]"));
    lines.push(String::from("  TuwaiqFS v3 (recoverable)"));
    match vfs::shell_pwd() {
        Ok(path) => {
            let mut line = String::from("  CWD: ");
            line.push_str(&path);
            lines.push(line);
        }
        Err(_) => lines.push(String::from("  CWD: unavailable")),
    }
    lines.push(String::from(""));

    lines.push(String::from("[ Network ]"));
    if net::is_initialized() {
        for status in net::status_lines() {
            let mut line = String::from("  ");
            line.push_str(&status);
            lines.push(line);
        }
    } else {
        lines.push(String::from("  Network: not initialized"));
    }

    Ok(lines)
}

fn format_u64_line(prefix: &str, value: u64, suffix: &str) -> String {
    let mut line = String::from(prefix);
    line.push_str(&u64_to_string(value));
    line.push_str(suffix);
    line
}

fn u64_to_string(mut value: u64) -> String {
    if value == 0 {
        return String::from("0");
    }
    let mut digits = Vec::new();
    while value > 0 {
        digits.push(b'0' + (value % 10) as u8);
        value /= 10;
    }
    digits.reverse();
    String::from(core::str::from_utf8(&digits).unwrap_or("0"))
}

fn u32_to_string(mut value: u32) -> String {
    if value == 0 {
        return String::from("0");
    }
    let mut digits = Vec::new();
    while value > 0 {
        digits.push(b'0' + (value % 10) as u8);
        value /= 10;
    }
    digits.reverse();
    String::from(core::str::from_utf8(&digits).unwrap_or("0"))
}
