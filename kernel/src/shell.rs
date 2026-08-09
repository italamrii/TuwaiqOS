//! Interactive shell for TuwaiqOS.
//!
//! Features: command history, arrow-key recall, tab completion, and the
//! `tuwaiq@os:~$` prompt.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use bootloader_api::{info::MemoryRegionKind, BootInfo};

use crate::ai_bridge;
use crate::allocator;
use crate::apps::{editor, monitor, notes};
use crate::interrupts;
use crate::keyboard::{poll_key, KeyEvent};
use crate::loader;
use crate::memory;
use crate::net;
use crate::paging;
use crate::reboot;
use crate::task;
use crate::vfs;

// Diagnostics compile the exact userspace window-policy source rather than
// maintaining a lookalike kernel test model. It is never used for rendering
// or policy in Ring 0; `desktoptest` only invokes its pure state self-test.
#[path = "../../userland/hello/src/bin/desktop/window.rs"]
#[allow(dead_code)]
mod desktop_window_model;

const MAX_LINE: usize = 128;
const HISTORY_SIZE: usize = 16;

/// Which console backend is active.
#[derive(Clone, Copy)]
pub enum ConsoleMode {
    Framebuffer,
    Vga,
    Serial,
}

struct History {
    entries: [[u8; MAX_LINE]; HISTORY_SIZE],
    count: usize,
    browse: usize,
}

impl History {
    const fn new() -> Self {
        Self {
            entries: [[0; MAX_LINE]; HISTORY_SIZE],
            count: 0,
            browse: 0,
        }
    }

    fn push(&mut self, line: &[u8], len: usize) {
        if len == 0 {
            return;
        }
        let index = self.count.min(HISTORY_SIZE - 1);
        self.entries[index][..len].copy_from_slice(&line[..len]);
        self.entries[index][len..].fill(0);
        if self.count < HISTORY_SIZE {
            self.count += 1;
        } else {
            for i in 1..HISTORY_SIZE {
                self.entries[i - 1] = self.entries[i];
            }
            self.entries[HISTORY_SIZE - 1][..len].copy_from_slice(&line[..len]);
        }
        self.browse = self.count;
    }

    fn recall_up(&mut self) -> Option<&[u8]> {
        if self.count == 0 {
            return None;
        }
        if self.browse == 0 {
            self.browse = 0;
        } else if self.browse > self.count {
            self.browse = self.count - 1;
        } else if self.browse > 0 {
            self.browse -= 1;
        }
        let entry = &self.entries[self.browse];
        let len = entry.iter().position(|&b| b == 0).unwrap_or(MAX_LINE);
        Some(&entry[..len])
    }

    fn recall_down(&mut self) -> Option<&[u8]> {
        if self.count == 0 {
            return None;
        }
        if self.browse + 1 >= self.count {
            self.browse = self.count;
            return Some(&[]);
        }
        self.browse += 1;
        let entry = &self.entries[self.browse];
        let len = entry.iter().position(|&b| b == 0).unwrap_or(MAX_LINE);
        Some(&entry[..len])
    }

    fn reset_browse(&mut self) {
        self.browse = self.count;
    }
}

/// Run the interactive shell forever.
pub fn run(boot_info: &'static BootInfo, mode: ConsoleMode) -> ! {
    let mut line = [0u8; MAX_LINE];
    let mut history = History::new();
    let mut first_prompt = true;

    // One-shot readiness summary: distinguishes shell/recovery entry from
    // scheduler heartbeat spam on COM1.
    let root_online = vfs::kind("/", "/").is_ok();
    let boot_online = vfs::kind("/", "/boot").is_ok();
    let storage = crate::storage::backend_name();
    crate::serial_println!(
        "shell: console-ready mode={} storage={} root={} boot={}",
        match mode {
            ConsoleMode::Framebuffer => "framebuffer",
            ConsoleMode::Vga => "vga",
            ConsoleMode::Serial => "serial-recovery",
        },
        storage,
        if root_online {
            "online"
        } else {
            "OFFLINE-recovery"
        },
        if boot_online { "online" } else { "UNAVAILABLE" }
    );
    if !root_online {
        println(mode, "RECOVERY CONSOLE: writable root offline");
        crate::serial_println!("shell: recovery console active; writable root offline");
    }

    loop {
        if !first_prompt {
            println(mode, "");
        }
        first_prompt = false;

        print_dynamic_prompt(mode);

        let mut len = 0;
        history.reset_browse();

        loop {
            match poll_key() {
                // Nothing queued: halt until the next interrupt (timer or
                // keyboard) instead of burning CPU re-checking the queue.
                // v0.5 busy-polled ports 0x60/0x64 directly in this spot.
                KeyEvent::None => interrupts::halt(),
                KeyEvent::Char(ch) => {
                    if len < MAX_LINE {
                        line[len] = ch;
                        len += 1;
                        print_char(mode, ch);
                    }
                }
                KeyEvent::Backspace => {
                    if len > 0 {
                        len -= 1;
                        backspace(mode);
                    }
                }
                KeyEvent::ArrowUp => {
                    if let Some(entry) = history.recall_up() {
                        len = replace_input(mode, &mut line, len, entry);
                    }
                }
                KeyEvent::ArrowDown => {
                    if let Some(entry) = history.recall_down() {
                        len = replace_input(mode, &mut line, len, entry);
                    }
                }
                KeyEvent::Tab => {
                    len = tab_complete(mode, &mut line, len);
                }
                KeyEvent::Escape => {}
                KeyEvent::Enter => {
                    println(mode, "");
                    let command = core::str::from_utf8(&line[..len]).unwrap_or("");
                    history.push(&line, len);
                    execute_command(boot_info, mode, command.trim());
                    line[..len].fill(0);
                    break;
                }
            }
        }
    }
}

fn replace_input(mode: ConsoleMode, line: &mut [u8], len: usize, new_text: &[u8]) -> usize {
    for _ in 0..len {
        backspace(mode);
    }
    let new_len = new_text.len().min(MAX_LINE);
    line[..new_len].copy_from_slice(&new_text[..new_len]);
    for &ch in &new_text[..new_len] {
        print_char(mode, ch);
    }
    new_len
}

fn tab_complete(mode: ConsoleMode, line: &mut [u8], len: usize) -> usize {
    let current = core::str::from_utf8(&line[..len]).unwrap_or("");
    let prefix = current.trim_end();
    if prefix.is_empty() {
        return len;
    }

    let (token, is_command) = if let Some(index) = prefix.rfind(' ') {
        (&prefix[index + 1..], false)
    } else {
        (prefix, true)
    };

    let mut matches = Vec::new();
    if is_command {
        for cmd in command_names() {
            if cmd.starts_with(token) {
                matches.push(String::from(*cmd));
            }
        }
        for name in loader::program_names() {
            if (*name).starts_with(token) {
                matches.push(String::from(*name));
            }
        }
    } else if prefix.starts_with("runelf ") {
        for name in embedded_program_names() {
            if name.starts_with(token) {
                matches.push(String::from(*name));
            }
        }
    }
    if let Ok(files) = vfs::completion_candidates(token) {
        for file in files {
            if file.starts_with(token) {
                matches.push(file);
            }
        }
    }

    if matches.is_empty() {
        return len;
    }

    if matches.len() == 1 {
        let prefix_owned = String::from(prefix);
        let token_owned = String::from(token);
        let completion = matches[0].clone();
        return apply_completion(mode, line, len, &prefix_owned, &token_owned, &completion);
    }

    println(mode, "");
    for m in &matches {
        println(mode, m);
    }
    print_dynamic_prompt(mode);
    for i in 0..len {
        print_char(mode, line[i]);
    }
    len
}

fn apply_completion(
    mode: ConsoleMode,
    line: &mut [u8],
    len: usize,
    prefix: &str,
    _token: &str,
    completion: &str,
) -> usize {
    let base = if let Some(index) = prefix.rfind(' ') {
        &prefix[..=index]
    } else {
        ""
    };
    let mut new_line = String::from(base);
    new_line.push_str(completion);
    if command_names().iter().any(|c| *c == completion) || base.is_empty() {
        new_line.push(' ');
    }
    replace_input(mode, line, len, new_line.as_bytes())
}

fn print_dynamic_prompt(mode: ConsoleMode) {
    let mut prompt = String::from("tuwaiq@os:");
    match vfs::shell_pwd() {
        Ok(path) if path == "/" => prompt.push_str("~"),
        Ok(path) => prompt.push_str(&path),
        Err(_) => prompt.push('~'),
    }
    prompt.push('$');
    prompt.push(' ');
    print(mode, &prompt);
}

fn execute_command(boot_info: &BootInfo, mode: ConsoleMode, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }

    crate::serial_println!("shell: command begin: {}", line);

    let (command, args) = split_command(line);

    match command {
        "help" => print_help(mode),
        "about" => {
            println(mode, "TuwaiqOS");
            println(
                mode,
                "Experimental AI-Native Operating System written in Rust.",
            );
        }
        "version" => println(mode, "TuwaiqOS v0.5"),
        "banner" => print_banner(mode),
        "sysinfo" => print_sysinfo(boot_info, mode),
        "uptime" => print_uptime(mode),
        "reboot" => reboot::system(),
        "clear" | "cls" => clear_screen(mode),
        "echo" => println(mode, args),
        "meminfo" => print_meminfo(boot_info, mode),
        "memtest" => match memory::memtest() {
            Ok(()) => println(mode, "Heap allocation test passed."),
            Err(reason) => {
                print(mode, "Heap allocation test failed: ");
                println(mode, reason);
            }
        },
        "pwd" => match vfs::shell_pwd() {
            Ok(path) => println(mode, &path),
            Err(reason) => print_fs_error(mode, reason),
        },
        "cd" => handle_cd(mode, args),
        "ls" => match vfs::shell_list(if args.trim().is_empty() {
            None
        } else {
            Some(args.trim())
        }) {
            Ok(entries) => print_entries(mode, entries),
            Err(reason) => print_fs_error(mode, reason),
        },
        "mounts" => handle_mounts(mode),
        "touch" => handle_touch(mode, args),
        "mkdir" => handle_mkdir(mode, args),
        "cat" => handle_cat(mode, args),
        "write" => handle_write(mode, args),
        "ps" => handle_ps(mode),
        "taskinfo" => handle_taskinfo(mode, args),
        "kill" => handle_kill(mode, args),
        "yield" => {
            task::yield_now();
            println(mode, "Yielded one time slice.");
        }
        "net" => handle_net_command(mode, args),
        "ping" => handle_ping(mode, args),
        "run" => handle_run(mode, args),
        "notes" => handle_notes(mode, args),
        "editor" => handle_editor(mode, args),
        "monitor" => handle_monitor(boot_info, mode),
        "runelf" => handle_runelf(mode, args),
        "runfs" => handle_runfs(mode, args),
        "installapp" => handle_install_app(mode, args),
        "vfstest" => handle_vfs_test(mode),
        "storagetest" => handle_storage_test(mode),
        "nvmetest" => handle_nvme_test(mode),
        "fsinterrupttest" => handle_fs_interrupted_write_test(mode),
        "fsexhausttest" => handle_fs_exhaustion_test(mode),
        "aipreviewtest" => handle_ai_preview_test(mode),
        "desktopaitest" => handle_desktop_ai_test(mode),
        "isolate" => handle_isolate(mode, args),
        "spawnfail" => handle_spawnfail(mode, args),
        "reap" => handle_reap(mode, args),
        "autoreap" => handle_automatic_reap(mode, args),
        "killreap" => handle_kill_reap(mode, args),
        "desktop" => handle_desktop(mode),
        "desktoppeer" => handle_desktop_peer(mode),
        "desktopfaultpeer" => handle_desktop_fault_peer(mode),
        "desktopkilltest" => handle_desktop_kill_test(mode),
        "desktopinteraction" => handle_desktop_interaction_test(mode),
        "desktopcycle" => handle_desktop_cycle(mode, args),
        "desktoptest" => handle_desktop_test(mode),
        "mousetest" => handle_mouse_test(mode),
        "desktopstats" => print_desktop_lifecycle_stats("manual"),
        "inputstats" => print_input_telemetry(mode, "manual"),
        "ai" => handle_ai_command(mode, line, args),
        "ask" => handle_ask_command(mode, args),
        _ => {
            print(mode, "Unknown command: ");
            println(mode, command);
        }
    }
    crate::serial_println!("shell: command complete: {}", line);
}

fn print_entries(mode: ConsoleMode, entries: Vec<String>) {
    if entries.is_empty() {
        println(mode, "(empty)");
    } else {
        for entry in entries {
            println(mode, &entry);
        }
    }
}

fn handle_mounts(mode: ConsoleMode) {
    match vfs::mounts() {
        Ok(mounts) => {
            for mount in mounts {
                print(mode, &mount.path);
                print(mode, "  ");
                print(mode, mount.label);
                println(mode, if mount.read_only { "  ro" } else { "  rw" });
            }
        }
        Err(reason) => print_fs_error(mode, reason),
    }
}

fn handle_run(mode: ConsoleMode, args: &str) {
    let name = args.trim();
    if name.is_empty() {
        println(mode, "Usage: run <program>");
        return;
    }
    match loader::run(name, "") {
        Ok(lines) => {
            for line in lines {
                println(mode, &line);
            }
        }
        Err(reason) => {
            print(mode, "Program error: ");
            println(mode, reason);
        }
    }
}

fn handle_notes(mode: ConsoleMode, args: &str) {
    let (sub, rest) = split_command(args);
    match notes::handle(sub, rest) {
        Ok(lines) => {
            for line in lines {
                println(mode, &line);
            }
        }
        Err(reason) => {
            print(mode, "Notes error: ");
            println(mode, reason);
        }
    }
}

fn handle_editor(mode: ConsoleMode, args: &str) {
    match editor::handle(args) {
        Ok(lines) => {
            for line in lines {
                println(mode, &line);
            }
        }
        Err(reason) => {
            print(mode, "Editor error: ");
            println(mode, reason);
        }
    }
}

fn handle_monitor(boot_info: &BootInfo, mode: ConsoleMode) {
    match monitor::snapshot(boot_info) {
        Ok(lines) => {
            for line in lines {
                println(mode, &line);
            }
        }
        Err(reason) => {
            print(mode, "Monitor error: ");
            println(mode, reason);
        }
    }
}

/// The six real, compiled ELF64 test programs (`userland/hello`), embedded
/// at build time -- see that crate's `src/bin/*.rs` for what each one
/// actually does. `hello` is the well-behaved one; the five `bad_*` binaries
/// each deliberately trigger one required fault-isolation category (see
/// `ARCHITECTURE.md`'s "User process fault isolation" section).
fn embedded_program(name: &str) -> Option<&'static [u8]> {
    match name {
        "hello" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/hello"
        ))),
        "bad_syscall" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/bad_syscall"
        ))),
        "bad_pointer" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/bad_pointer"
        ))),
        "bad_privileged" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/bad_privileged"
        ))),
        "bad_kernel" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/bad_kernel"
        ))),
        "bad_unmapped" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/bad_unmapped"
        ))),
        "bad_ud2" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/bad_ud2"
        ))),
        "bad_divzero" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/bad_divzero"
        ))),
        "bad_mmap" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/bad_mmap"
        ))),
        "bad_munmap" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/bad_munmap"
        ))),
        "bad_display" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/bad_display"
        ))),
        "bad_input" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/bad_input"
        ))),
        "bad_net" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/bad_net"
        ))),
        "mmap_ro_fault" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/mmap_ro_fault"
        ))),
        "mmap_nx_fault" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/mmap_nx_fault"
        ))),
        "post_unmap_fault" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/post_unmap_fault"
        ))),
        "mmap_exhaustion" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/mmap_exhaustion"
        ))),
        "mmap_partial_failure" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/mmap_partial_failure"
        ))),
        "desktop" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/desktop"
        ))),
        "desktop_peer" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/desktop_peer"
        ))),
        "file_api_test" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/file_api_test"
        ))),
        "file_mutation_test" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/file_mutation_test"
        ))),
        "tuwaiq_ai" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/tuwaiq_ai"
        ))),
        "tuwaiq_ai_fault" => Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/x86_64-unknown-none/release/tuwaiq_ai_fault"
        ))),
        _ => None,
    }
}

fn embedded_program_names() -> &'static [&'static str] {
    &[
        "hello",
        "bad_syscall",
        "bad_pointer",
        "bad_privileged",
        "bad_kernel",
        "bad_unmapped",
        "bad_ud2",
        "bad_divzero",
        "bad_mmap",
        "bad_munmap",
        "bad_display",
        "bad_input",
        "bad_net",
        "mmap_ro_fault",
        "mmap_nx_fault",
        "post_unmap_fault",
        "mmap_exhaustion",
        "mmap_partial_failure",
        "desktop",
        "desktop_peer",
        "file_api_test",
        "file_mutation_test",
        "tuwaiq_ai",
        "tuwaiq_ai_fault",
    ]
}

fn wait_for_terminated(id: u32) {
    loop {
        match task::info(id) {
            Ok(t) if t.state == task::TaskState::Terminated => break,
            Ok(_) => task::yield_now(),
            Err(_) => break,
        }
    }
}

fn print_process_result(mode: ConsoleMode, id: u32) {
    match task::info(id) {
        Ok(t) => {
            crate::serial_println!(
                "process: result pid={} name={} state={} exit_code={}",
                t.id,
                t.name,
                task::state_label(t.state),
                t.exit_code.map(i64::from).unwrap_or(-1)
            );
            print(mode, "  pid=");
            print_u64(mode, t.id as u64);
            print(mode, " name=");
            print(mode, &t.name);
            print(mode, " state=");
            print(mode, task::state_label(t.state));
            print(mode, " exit_code=");
            match t.exit_code {
                Some(code) => print_i64(mode, code as i64),
                None => print(mode, "none"),
            }
            println(mode, "");
        }
        Err(reason) => {
            print(mode, "  task error: ");
            println(mode, reason);
        }
    }
}

fn ensure_apps_directory() -> Result<(), &'static str> {
    match vfs::kind("/", "/apps") {
        Ok(vfs::NodeKind::Directory) => Ok(()),
        Ok(vfs::NodeKind::File) => Err("/apps is not a directory"),
        Err(_) => vfs::create_dir("/", "/apps"),
    }
}

fn ensure_directory(path: &str) -> Result<(), &'static str> {
    match vfs::kind("/", path) {
        Ok(vfs::NodeKind::Directory) => Ok(()),
        Ok(vfs::NodeKind::File) => Err("expected directory is a file"),
        Err(_) => vfs::create_dir("/", path),
    }
}

fn ensure_app_data_directory(name: &str) -> Result<(), &'static str> {
    ensure_directory("/data")?;
    let mut path = String::from("/data/");
    path.try_reserve_exact(name.len())
        .map_err(|_| "application data path allocation failed")?;
    path.push_str(name);
    ensure_directory(&path)
}

fn install_app(name: &str) -> Result<&'static str, &'static str> {
    let (embedded_name, target) = match name {
        "hello" => ("hello", "/apps/hello"),
        "file-api-test" => ("file_api_test", "/apps/file-api-test"),
        "file-mutation-test" => ("file_mutation_test", "/apps/file-mutation-test"),
        "tuwaiq-ai" => ("tuwaiq_ai", "/apps/tuwaiq-ai"),
        "tuwaiq-ai-fault" => ("tuwaiq_ai_fault", "/apps/tuwaiq-ai-fault"),
        _ => return Err("unknown provisionable application"),
    };
    let bytes = embedded_program(embedded_name).ok_or("embedded bootstrap image missing")?;
    ensure_apps_directory()?;
    vfs::write_file("/", target, bytes)?;
    ensure_app_data_directory(vfs::basename(target))?;
    Ok(target)
}

fn handle_install_app(mode: ConsoleMode, args: &str) {
    let name = args.trim();
    if name.is_empty() {
        println(
            mode,
            "Usage: installapp <hello|file-api-test|file-mutation-test|tuwaiq-ai|tuwaiq-ai-fault>",
        );
        return;
    }
    match install_app(name) {
        Ok(path) => {
            print(mode, "Installed bootstrap application at ");
            println(mode, path);
        }
        Err(reason) => print_fs_error(mode, reason),
    }
}

fn handle_storage_test(mode: ConsoleMode) {
    let path = match install_app("file-mutation-test") {
        Ok(path) => path,
        Err(reason) => {
            print_fs_error(mode, reason);
            return;
        }
    };
    let id = match spawn_from_vfs(path, "/") {
        Ok(id) => id,
        Err(reason) => {
            print(mode, "storage: FAIL filesystem-backed spawn: ");
            println(mode, reason);
            return;
        }
    };
    wait_for_terminated(id);
    let exit = task::info(id).ok().and_then(|info| info.exit_code);
    task::reap_now();
    println(
        mode,
        if exit == Some(0) {
            "storage: PASS Ring-3 mutation ABI and filesystem-backed ELF"
        } else {
            "storage: FAIL Ring-3 mutation process"
        },
    );
}

fn handle_nvme_test(mode: ConsoleMode) {
    print(mode, "NVMe boot backend: ");
    println(mode, crate::storage::backend_name());
    match crate::storage::nvme_self_test() {
        Ok(report)
            if report.invalid_lba_rejected
                && report.malformed_namespace_rejected
                && report.timeout_bounded
                && report.reset_recovered
                && report.claims_stable
                && report.frames_stable
                && report.heap_stable =>
        {
            print(
                mode,
                "nvme-test: PASS invalid-lba malformed-namespace timeout-reset no-leak sectors=",
            );
            print_u64(mode, report.sector_count);
            println(mode, "");
        }
        Ok(_) => println(mode, "nvme-test: FAIL incomplete invariant result"),
        Err(reason) => {
            print(mode, "nvme-test: FAIL ");
            println(mode, reason);
        }
    }
}

fn handle_fs_interrupted_write_test(mode: ConsoleMode) {
    if let Err(reason) = ensure_directory("/data/recovery") {
        print_fs_error(mode, reason);
        return;
    }
    let path = "/data/recovery/interrupted.txt";
    let stable = b"stable-before-power-loss";
    if let Err(reason) = vfs::write_file("/", path, stable) {
        print_fs_error(mode, reason);
        return;
    }
    if let Err(reason) =
        vfs::inject_interrupted_write("/", path, b"uncommitted-after-power-loss", 1)
    {
        print(mode, "fs-recovery: FAIL injection: ");
        println(mode, reason);
        return;
    }
    let unchanged = vfs::read_file("/", path)
        .map(|bytes| bytes.as_ref() == stable)
        .unwrap_or(false);
    println(
        mode,
        if unchanged {
            "fs-recovery: PASS interrupted checkpoint rejected; active data unchanged"
        } else {
            "fs-recovery: FAIL active data changed after interrupted checkpoint"
        },
    );
}

fn handle_fs_exhaustion_test(mode: ConsoleMode) {
    if let Err(reason) = ensure_directory("/data/exhaust") {
        print_fs_error(mode, reason);
        return;
    }
    for path in [
        "/data/exhaust/a.bin",
        "/data/exhaust/b.bin",
        "/data/exhaust/c.bin",
        "/data/exhaust/sentinel.txt",
        "/data/exhaust/reused.txt",
    ] {
        let _ = vfs::remove("/", path);
    }
    if let Err(reason) = vfs::write_file("/", "/data/exhaust/sentinel.txt", b"preserved") {
        print_fs_error(mode, reason);
        return;
    }
    let mut block = Vec::new();
    if block
        .try_reserve_exact(crate::tuwaiqfs::MAX_FILE_SIZE)
        .is_err()
    {
        println(mode, "fs-exhaustion: FAIL allocation");
        return;
    }
    block.resize(crate::tuwaiqfs::MAX_FILE_SIZE, 0xA5);
    if let Err(reason) = vfs::write_file("/", "/data/exhaust/a.bin", &block) {
        print_fs_error(mode, reason);
        return;
    }
    block.fill(0x5A);
    if let Err(reason) = vfs::write_file("/", "/data/exhaust/b.bin", &block) {
        print_fs_error(mode, reason);
        return;
    }
    block.fill(0x3C);
    let rejected = matches!(
        vfs::write_file("/", "/data/exhaust/c.bin", &block),
        Err("filesystem metadata too large")
    );
    let absent = vfs::kind("/", "/data/exhaust/c.bin").is_err();
    let preserved = vfs::read_file("/", "/data/exhaust/sentinel.txt")
        .map(|bytes| bytes.as_ref() == b"preserved")
        .unwrap_or(false);
    let cleanup = vfs::remove("/", "/data/exhaust/a.bin")
        .and_then(|()| vfs::remove("/", "/data/exhaust/b.bin"));
    let reused = cleanup
        .and_then(|()| vfs::write_file("/", "/data/exhaust/reused.txt", b"space-reused"))
        .and_then(|()| vfs::read_file("/", "/data/exhaust/reused.txt"))
        .map(|bytes| bytes.as_ref() == b"space-reused")
        .unwrap_or(false);
    println(
        mode,
        if rejected && absent && preserved && reused {
            "fs-exhaustion: PASS atomic rejection preserved prior data and reclaimed space reused"
        } else {
            "fs-exhaustion: FAIL atomicity or reuse"
        },
    );
}

fn spawn_from_vfs(path: &str, cwd: &str) -> Result<u32, &'static str> {
    let absolute = vfs::normalize(cwd, path)?;
    let bytes = vfs::read_file("/", &absolute)?;
    if bytes.is_empty() || bytes.len() > vfs::MAX_EXECUTABLE_SIZE {
        return Err("invalid executable file size");
    }
    task::spawn_user_process_with_cwd(vfs::basename(&absolute), &bytes, cwd)
}

fn handle_runfs(mode: ConsoleMode, args: &str) {
    let path = args.trim();
    if path.is_empty() {
        println(mode, "Usage: runfs <path>");
        return;
    }
    let Ok(cwd) = vfs::shell_pwd() else {
        print_fs_error(mode, "VFS not initialized");
        return;
    };
    match spawn_from_vfs(path, &cwd) {
        Ok(id) => {
            print(mode, "Spawned filesystem ELF pid ");
            print_u64(mode, id as u64);
            println(mode, "");
            wait_for_terminated(id);
            print_process_result(mode, id);
        }
        Err(reason) => {
            print(mode, "Filesystem process load error: ");
            println(mode, reason);
        }
    }
}

fn handle_vfs_test(mode: ConsoleMode) {
    if let Err(reason) = vfs::self_test() {
        print(mode, "vfs: FAIL path semantics: ");
        println(mode, reason);
        return;
    }
    match vfs::kind("/", "/phase6-test") {
        Ok(vfs::NodeKind::Directory) => {}
        Ok(vfs::NodeKind::File) => {
            println(mode, "vfs: FAIL /phase6-test is a file");
            return;
        }
        Err(_) => {
            if let Err(reason) = vfs::create_dir("/", "/phase6-test") {
                print_fs_error(mode, reason);
                return;
            }
        }
    }
    if let Err(reason) = vfs::write_file("/", "/phase6-test/data.txt", b"phase6-data") {
        print_fs_error(mode, reason);
        return;
    }
    let path = match install_app("file-api-test") {
        Ok(path) => path,
        Err(reason) => {
            print_fs_error(mode, reason);
            return;
        }
    };
    let warmup = match spawn_from_vfs(path, "/phase6-test") {
        Ok(id) => id,
        Err(reason) => {
            print(mode, "vfs: FAIL filesystem-backed warmup: ");
            println(mode, reason);
            return;
        }
    };
    wait_for_terminated(warmup);
    if task::info(warmup).ok().and_then(|info| info.exit_code) != Some(0) {
        println(mode, "vfs: FAIL warmup exit");
        return;
    }
    task::reap_now();
    let before = desktop_lifecycle_stats();
    let id = match spawn_from_vfs(path, "/phase6-test") {
        Ok(id) => id,
        Err(reason) => {
            print(mode, "vfs: FAIL filesystem-backed spawn: ");
            println(mode, reason);
            return;
        }
    };
    wait_for_terminated(id);
    let exit = task::info(id).ok().and_then(|info| info.exit_code);
    task::reap_now();
    let after = desktop_lifecycle_stats();
    let resources_ok = before.tasks == after.tasks
        && before.frame_bump == after.frame_bump
        && before.live_frames == after.live_frames
        && before.heap_used == after.heap_used;
    println(
        mode,
        &format!(
            "vfs: {} mount-table fat32-readonly normalized-paths per-process-cwd read-handles seek filesystem-elf exit-cleanup exit_code={} tasks={}->{} frames={}->{} bump={}->{} heap={}->{}",
            if exit == Some(0) && resources_ok { "PASS" } else { "FAIL" },
            exit.map(i64::from).unwrap_or(-1),
            before.tasks,
            after.tasks,
            before.live_frames,
            after.live_frames,
            before.frame_bump,
            after.frame_bump,
            before.heap_used,
            after.heap_used
        ),
    );
}

fn run_vfs_process_to_exit(path: &str, expected: i32) -> Result<bool, &'static str> {
    let id = spawn_from_vfs(path, "/")?;
    wait_for_terminated(id);
    Ok(task::info(id).ok().and_then(|info| info.exit_code) == Some(expected))
}

fn handle_ai_preview_test(mode: ConsoleMode) {
    let good = match install_app("tuwaiq-ai") {
        Ok(path) => path,
        Err(reason) => {
            print_fs_error(mode, reason);
            return;
        }
    };
    let fault = match install_app("tuwaiq-ai-fault") {
        Ok(path) => path,
        Err(reason) => {
            print_fs_error(mode, reason);
            return;
        }
    };

    // Warm every measured process/fault shape before taking a leak baseline.
    // The first CPL3 exception path grows small reusable formatter/allocator
    // state, so warming only the successful provider makes that one-time
    // allocation look like a per-cycle leak.
    if run_vfs_process_to_exit(good, 0) != Ok(true)
        || run_vfs_process_to_exit(fault, 132) != Ok(true)
        || run_vfs_process_to_exit(good, 0) != Ok(true)
    {
        println(mode, "ai-preview: FAIL warmup");
        return;
    }
    task::reap_now();
    let before = desktop_lifecycle_stats();
    let behavior_ok = run_vfs_process_to_exit(good, 0) == Ok(true)
        && run_vfs_process_to_exit(fault, 132) == Ok(true)
        && run_vfs_process_to_exit(good, 0) == Ok(true);
    task::reap_now();
    let after = desktop_lifecycle_stats();
    let resources_ok = before.tasks == after.tasks
        && before.frame_bump == after.frame_bump
        && before.live_frames == after.live_frames
        && before.heap_used == after.heap_used;
    println(
        mode,
        &format!(
            "ai-preview: {} ring3 lifecycle crash-isolation relaunch resources tasks={}->{} frames={}->{} bump={}->{} heap={}->{}",
            if behavior_ok && resources_ok { "PASS" } else { "FAIL" },
            before.tasks,
            after.tasks,
            before.live_frames,
            after.live_frames,
            before.frame_bump,
            after.frame_bump,
            before.heap_used,
            after.heap_used
        ),
    );
}

fn handle_desktop_ai_test(mode: ConsoleMode) {
    if let Err(reason) = install_app("tuwaiq-ai") {
        print_fs_error(mode, reason);
        return;
    }
    let Some(desktop) = embedded_program("desktop") else {
        println(mode, "desktop AI preview: FAIL embedded desktop missing");
        return;
    };

    // Warm the complete concurrent shape, not merely each process in
    // isolation. Its first peak may grow reusable frame/allocator pools.
    if run_desktop_ai_cycle(desktop, false).is_err() {
        println(mode, "desktop AI preview: FAIL warmup");
        return;
    }
    task::reap_now();
    let before = desktop_lifecycle_stats();
    let Ok(cycle) = run_desktop_ai_cycle(desktop, true) else {
        println(mode, "desktop AI preview: FAIL measured cycle");
        return;
    };
    task::reap_now();
    let after = desktop_lifecycle_stats();
    let resources_ok = before.tasks == after.tasks
        && before.frame_bump == after.frame_bump
        && before.live_frames == after.live_frames
        && before.heap_used == after.heap_used;
    println(
        mode,
        &format!(
            "desktop AI preview: {} click={} service_exit={:?} desktop_exit={:?} dropped={} tasks={}->{} frames={}->{} bump={}->{} heap={}->{}",
            if cycle.click_ok
                && cycle.service_exit == Some(0)
                && cycle.desktop_exit == Some(0)
                && cycle.dropped_events == 0
                && resources_ok
            {
                "PASS"
            } else {
                "FAIL"
            },
            cycle.click_ok,
            cycle.service_exit,
            cycle.desktop_exit,
            cycle.dropped_events,
            before.tasks,
            after.tasks,
            before.live_frames,
            after.live_frames,
            before.frame_bump,
            after.frame_bump,
            before.heap_used,
            after.heap_used
        ),
    );
}

struct DesktopAiCycle {
    click_ok: bool,
    service_exit: Option<i32>,
    desktop_exit: Option<i32>,
    dropped_events: u64,
}

fn run_desktop_ai_cycle(
    desktop: &'static [u8],
    expose_evidence_window: bool,
) -> Result<DesktopAiCycle, &'static str> {
    let presents_before = crate::display::present_telemetry().count;
    let desktop_id = spawn_foreground_process("desktop-ai-preview", desktop)?;
    if !wait_for_desktop_present(desktop_id, presents_before, 500) {
        let _ = task::kill(desktop_id);
        let _ = release_desktop_foreground(desktop_id);
        task::reap_now();
        return Err("desktop first present timed out");
    }

    let click = x86_64::instructions::interrupts::without_interrupts(|| {
        inject_mouse_to(160, 12, 0)?;
        crate::mouse::inject_screen_packet(0, 0, 1)?;
        crate::mouse::inject_screen_packet(0, 0, 0)
    });
    let deadline = interrupts::ticks().saturating_add(500);
    let mut service_exit = None;
    while interrupts::ticks() < deadline {
        service_exit = task::list().ok().and_then(|tasks| {
            tasks
                .into_iter()
                .find(|entry| entry.name == "tuwaiq-ai")
                .and_then(|entry| entry.exit_code)
        });
        if service_exit.is_some() {
            break;
        }
        task::yield_now();
    }
    if expose_evidence_window {
        crate::serial_println!(
            "desktop-ai-preview: window visible service_exit={:?}",
            service_exit
        );
        // Keep the real preview visible long enough for the project-side
        // harness to capture it before requesting normal desktop exit.
        task::sleep_ticks(200);
    }
    crate::input::push_key_event(KeyEvent::Escape);
    wait_for_terminated(desktop_id);
    let desktop_exit = task::info(desktop_id).ok().and_then(|info| info.exit_code);
    let telemetry = release_desktop_foreground(desktop_id);
    Ok(DesktopAiCycle {
        click_ok: click.is_ok(),
        service_exit,
        desktop_exit,
        dropped_events: telemetry.dropped_events,
    })
}

/// `runelf <name>` -- loads one of the embedded ELF64 test programs as a
/// real user process (`task::spawn_user_process`, which goes through the
/// genuine ELF loader in `elf.rs`) and waits for it to terminate. The
/// reported result reflects the process's actual final state (`ps`
/// under the hood), not a fixed string; the detailed evidence of what
/// happened while it ran (Ring 3 entry, syscalls, any fault) is on the
/// serial log.
fn handle_runelf(mode: ConsoleMode, args: &str) {
    let name = args.trim();
    if name == "desktop" {
        handle_desktop(mode);
        return;
    }
    if name == "bad_input" {
        handle_input_security_test(mode);
        return;
    }
    if name == "bad_display" {
        handle_display_atomicity_test(mode);
        return;
    }
    if name == "mmap_exhaustion" {
        handle_mmap_exhaustion_test(mode);
        return;
    }
    if name == "mmap_partial_failure" {
        handle_mmap_partial_failure_test(mode);
        return;
    }
    let Some(bytes) = embedded_program(name) else {
        println(
            mode,
            "Usage: runelf <hello|bad_syscall|bad_pointer|bad_privileged|bad_kernel|bad_unmapped|\
             bad_ud2|bad_divzero|bad_mmap|bad_munmap|bad_display|bad_input|bad_net|mmap_ro_fault|\
             mmap_nx_fault|post_unmap_fault|mmap_exhaustion|mmap_partial_failure|desktop|desktop_peer>",
        );
        return;
    };

    match task::spawn_user_process(name, bytes) {
        Ok(id) => {
            print(mode, "Spawned pid ");
            print_u64(mode, id as u64);
            print(mode, " (");
            print(mode, name);
            println(mode, "), waiting for it to finish...");
            wait_for_terminated(id);
            print_process_result(mode, id);
        }
        Err(reason) => {
            print(mode, "Process load error: ");
            println(mode, reason);
        }
    }
}

/// Launch `bad_input` as the actual foreground PID and seed one known event.
/// Its invalid-pointer polls must leave this record queued for the final
/// valid poll, proving validation happens before consumption.
fn handle_input_security_test(mode: ConsoleMode) {
    let Some(bytes) = embedded_program("bad_input") else {
        println(mode, "bad_input: embedded program missing");
        return;
    };
    let spawn = x86_64::instructions::interrupts::without_interrupts(|| {
        let id = task::spawn_user_process("bad_input", bytes)?;
        acquire_desktop_foreground(id);
        crate::input::push_key_event(KeyEvent::Char(b'Z'));
        crate::serial_println!("input: security seeded pid={} tag=1 code=90", id);
        Ok(id)
    });
    match spawn {
        Ok(id) => {
            print(mode, "Spawned foreground input-security pid ");
            print_u64(mode, id as u64);
            println(mode, ", waiting for deterministic checks...");
            wait_for_terminated(id);
            let telemetry = release_desktop_foreground(id);
            emit_input_telemetry("bad-input-release", telemetry);
            print_process_result(mode, id);
        }
        Err(reason) => {
            print(mode, "Process load error: ");
            println(mode, reason);
        }
    }
}

/// Verify a rejected cross-page `DISPLAY_PRESENT` leaves every framebuffer
/// byte unchanged. The baseline is deliberately taken after the last
/// pre-spawn console write, and the result is sampled before any post-run
/// console output can perturb the framebuffer.
fn handle_display_atomicity_test(mode: ConsoleMode) {
    let Some(bytes) = embedded_program("bad_display") else {
        println(mode, "bad_display: embedded program missing");
        return;
    };
    println(
        mode,
        "Running bad_display with framebuffer atomicity checksum...",
    );
    let Some(checksum_before) = crate::display::framebuffer_checksum() else {
        println(mode, "display atomicity: FAIL framebuffer unavailable");
        return;
    };

    let spawn: Result<u32, &'static str> =
        x86_64::instructions::interrupts::without_interrupts(|| {
            let id = task::spawn_user_process("bad_display", bytes)?;
            acquire_desktop_foreground(id);
            Ok(id)
        });
    let result = match spawn {
        Ok(id) => {
            wait_for_terminated(id);
            let _ = release_desktop_foreground(id);
            let checksum_after = crate::display::framebuffer_checksum();
            Some((id, checksum_after))
        }
        Err(_) => None,
    };

    match result {
        Some((id, Some(checksum_after))) => {
            println(
                mode,
                &format!(
                    "display atomicity: {} checksum_before={:#x} checksum_after={:#x}",
                    if checksum_before == checksum_after {
                        "PASS"
                    } else {
                        "FAIL"
                    },
                    checksum_before,
                    checksum_after
                ),
            );
            print_process_result(mode, id);
        }
        Some((id, None)) => {
            println(mode, "display atomicity: FAIL framebuffer disappeared");
            print_process_result(mode, id);
        }
        None => println(mode, "display atomicity: FAIL process load error"),
    }
}

fn handle_mmap_exhaustion_test(mode: ConsoleMode) {
    let Some(bytes) = embedded_program("mmap_exhaustion") else {
        println(mode, "mmap_exhaustion: embedded program missing");
        return;
    };
    println(
        mode,
        "Running mmap exhaustion, rollback, and frame-reuse test...",
    );
    task::reap_now();
    let before = frame_reuse_stats();
    let id = match task::spawn_user_process("mmap_exhaustion", bytes) {
        Ok(id) => id,
        Err(reason) => {
            print(mode, "Process load error: ");
            println(mode, reason);
            return;
        }
    };
    task::reset_vm_batch_telemetry();
    wait_for_terminated(id);
    let exit_code = task::info(id).ok().and_then(|info| info.exit_code);
    task::reap_now();
    let after = frame_reuse_stats();
    let passed = exit_code == Some(0) && before.live == after.live;
    println(
        mode,
        &format!(
            "mmap exhaustion cleanup: {} exit_code={} live_before={} live_after={} bump_before={} bump_after={} free_before={} free_after={}",
            if passed { "PASS" } else { "FAIL" },
            exit_code.map(i64::from).unwrap_or(-1),
            before.live,
            after.live,
            before.bump,
            after.bump,
            before.free,
            after.free
        ),
    );
    let vm = task::vm_batch_telemetry();
    let cycles_per_tick = crate::interrupts::average_tsc_cycles_per_tick().unwrap_or(0);
    let max_milli_ticks = if cycles_per_tick == 0 {
        0
    } else {
        vm.max_cycles.saturating_mul(1000) / cycles_per_tick
    };
    println(
        mode,
        &format!(
            "vm batch telemetry: count={} total_cycles={} average_cycles={} max_cycles={} max_kind={} cycles_per_tick={} max_milli_ticks={} pages_per_batch={}",
            vm.count,
            vm.total_cycles,
            if vm.count == 0 { 0 } else { vm.total_cycles / vm.count },
            vm.max_cycles,
            vm.max_kind,
            cycles_per_tick,
            max_milli_ticks,
            task::VM_BATCH_PAGES
        ),
    );
}

fn handle_mmap_partial_failure_test(mode: ConsoleMode) {
    let Some(bytes) = embedded_program("mmap_partial_failure") else {
        println(mode, "mmap_partial_failure: embedded program missing");
        return;
    };
    task::reap_now();
    let before = frame_reuse_stats();
    task::inject_next_mmap_failure_after(4);
    let id = match task::spawn_user_process("mmap_partial_failure", bytes) {
        Ok(id) => id,
        Err(reason) => {
            task::clear_mmap_failure_injection();
            print(mode, "Process load error: ");
            println(mode, reason);
            return;
        }
    };
    wait_for_terminated(id);
    // The ELF normally consumes the hook. Clear it defensively if it exited
    // before reaching MMAP so no later process inherits test-only state.
    task::clear_mmap_failure_injection();
    let exit_code = task::info(id).ok().and_then(|info| info.exit_code);
    task::reap_now();
    let after = frame_reuse_stats();
    let passed = exit_code == Some(0) && before.live == after.live;
    println(
        mode,
        &format!(
            "mmap partial rollback: {} exit_code={} live_before={} live_after={} bump_before={} bump_after={}",
            if passed { "PASS" } else { "FAIL" },
            exit_code.map(i64::from).unwrap_or(-1),
            before.live,
            after.live,
            before.bump,
            after.bump
        ),
    );
}

#[derive(Clone, Copy)]
struct FrameReuseStats {
    bump: usize,
    free: usize,
    live: usize,
}

fn frame_reuse_stats() -> FrameReuseStats {
    let (bump, free) = paging::frame_stats()
        .map(|stats| (stats.bumped, stats.free_in_pool))
        .unwrap_or((0, 0));
    FrameReuseStats {
        bump,
        free,
        live: bump.saturating_sub(free),
    }
}

/// `isolate [program_b]` -- the Phase 4 process-isolation proof. Spawns
/// *two* processes back to back (both `Ready` and interleaving under the
/// same 100 Hz timer preemption everything else in this kernel runs under
/// -- neither is waited on before the other starts): `hello` and, by
/// default, a second independent `hello` instance -- or, if `program_b` is
/// given (e.g. `isolate bad_privileged`), that program instead, which lets
/// this same command double as the "faulting one process does not kill
/// another" proof: pid A keeps running and exits cleanly regardless of
/// what happens to pid B.
///
/// Prints hardware evidence that the two are genuinely separate: each
/// process's own PML4 physical address (`task::process_pml4_phys`) and
/// owned physical frame count (`task::process_frame_count`) -- two
/// different page-table roots is the actual mechanism behind "process A
/// cannot read process B's memory," not a claim this command makes on its
/// own.
fn handle_isolate(mode: ConsoleMode, args: &str) {
    let name_b = {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            "hello"
        } else {
            trimmed
        }
    };
    let (Some(bytes_a), Some(bytes_b)) = (embedded_program("hello"), embedded_program(name_b))
    else {
        println(
            mode,
            "Usage: isolate [bad_syscall|bad_pointer|bad_privileged|bad_kernel|bad_unmapped|bad_ud2|bad_divzero]",
        );
        return;
    };

    // Capture each process's PML4/frame-count evidence *immediately* after
    // spawning it -- not after printing anything. Printing goes through
    // the framebuffer/VGA character-by-character path, which takes long
    // enough in wall-clock terms that the 100 Hz timer can (and, for a
    // process as short-lived as `hello`, reliably does) preempt into it,
    // let it run to completion, and free its address space before this
    // function would otherwise have gotten around to reading it -- which
    // would print a reclaimed `PML4=0x0` instead of real evidence. This
    // way the values printed below are always the genuine snapshot taken
    // right as each process started, regardless of how fast it finishes.
    let Ok(id_a) = task::spawn_user_process("hello-a", bytes_a) else {
        print(mode, "Process load error spawning hello-a");
        println(mode, "");
        return;
    };
    let pml4_a = task::process_pml4_phys(id_a).unwrap_or(0);
    let frames_a = task::process_frame_count(id_a).unwrap_or(0);

    let Ok(id_b) = task::spawn_user_process(name_b, bytes_b) else {
        print(mode, "Process load error spawning ");
        println(mode, name_b);
        return;
    };
    let pml4_b = task::process_pml4_phys(id_b).unwrap_or(0);
    let frames_b = task::process_frame_count(id_b).unwrap_or(0);

    print(mode, "Spawned pid ");
    print_u64(mode, id_a as u64);
    print(mode, " (hello-a) and pid ");
    print_u64(mode, id_b as u64);
    print(mode, " (");
    print(mode, name_b);
    println(mode, "), running concurrently under preemption.");

    print(mode, "  pid ");
    print_u64(mode, id_a as u64);
    print(mode, " PML4=");
    print_hex(mode, pml4_a);
    print(mode, " frames=");
    print_u64(mode, frames_a as u64);
    println(mode, "");

    print(mode, "  pid ");
    print_u64(mode, id_b as u64);
    print(mode, " PML4=");
    print_hex(mode, pml4_b);
    print(mode, " frames=");
    print_u64(mode, frames_b as u64);
    println(mode, "");

    if pml4_a != 0 && pml4_a == pml4_b {
        println(
            mode,
            "  WARNING: both processes report the SAME PML4 -- address spaces are NOT isolated!",
        );
    } else if pml4_a != 0 && pml4_b != 0 {
        println(
            mode,
            "  Confirmed: distinct PML4 physical addresses -- genuinely separate page tables.",
        );
    }

    wait_for_terminated(id_a);
    wait_for_terminated(id_b);
    println(mode, "Both finished:");
    print_process_result(mode, id_a);
    print_process_result(mode, id_b);
}

/// Deliberately malformed ELF bytes -- too small to even contain a full
/// header (`elf.rs`'s very first bounds check) -- so every
/// `task::spawn_user_process` call in `handle_spawnfail` below fails at
/// the earliest possible point *after* `paging::new_address_space` has
/// already allocated a real PML4 frame for it. This is exactly the
/// scenario the address-space-cleanup fix targets: does that frame (and
/// nothing else) come back, every single time, or does it leak.
const MALFORMED_ELF: &[u8] = &[0x7f, b'E', b'L', b'F'];

/// `spawnfail <count>` -- the Phase 4 frame-reclamation proof. Calls
/// `task::spawn_user_process` with deliberately malformed ELF bytes
/// `count` times in a row, each one expected to fail cleanly, and compares
/// physical-frame accounting (`paging::frame_stats`) before and after: if
/// every failed spawn's frames were genuinely reclaimed rather than
/// leaked, "frames currently in use" (`allocated - free_in_pool`) is
/// identical before and after, no matter how many attempts ran in
/// between -- not a claim, a number printed from live allocator state.
fn handle_spawnfail(mode: ConsoleMode, args: &str) {
    let count: u32 = match args.trim().parse() {
        Ok(n) if n > 0 => n,
        _ => {
            println(mode, "Usage: spawnfail <count>");
            return;
        }
    };

    // One throwaway failed spawn first, *before* the measured loop, so the
    // free list already holds a reusable frame (or several) before
    // measurement starts. Without this warm-up, the very first measured
    // iteration would be forced to bump fresh memory no matter what (the
    // free list starts empty), making even a perfectly leak-free run look
    // like it grew by one -- see `paging::BootInfoFrameAllocator::frames_bumped`'s
    // docs for the full reasoning on why the bump cursor, not
    // `allocated - free_in_pool`, is the metric that's actually immune to
    // this: `allocated` counts every *call* to `allocate_frame`, including
    // ones satisfied by reusing an already-freed frame, so it grows by one
    // on every single iteration below regardless of whether anything
    // leaked -- comparing it before/after would report a "leak" every
    // time, even when frames are being perfectly recycled.
    let _ = task::spawn_user_process("bad-elf-warmup", MALFORMED_ELF);

    let Some(before) = paging::frame_stats() else {
        println(mode, "Frame stats unavailable (paging not active)");
        return;
    };
    print(mode, "Frame bump cursor before: ");
    print_u64(mode, before.bumped as u64);
    println(mode, "");

    let mut failures = 0u32;
    let mut unexpected_ok = 0u32;
    for _ in 0..count {
        match task::spawn_user_process("bad-elf", MALFORMED_ELF) {
            Err(_) => failures += 1,
            Ok(id) => {
                // Should never happen -- MALFORMED_ELF is deliberately
                // invalid -- but if it somehow did load, don't leave a
                // live task behind uncounted; note it and move on.
                unexpected_ok += 1;
                let _ = task::kill(id);
            }
        }
    }

    let Some(after) = paging::frame_stats() else {
        println(
            mode,
            "Frame stats unavailable after the loop (paging not active)",
        );
        return;
    };
    print(mode, "Frame bump cursor after ");
    print_u64(mode, count as u64);
    print(mode, " failed spawns: ");
    print_u64(mode, after.bumped as u64);
    println(mode, "");
    print(mode, "  failed as expected: ");
    print_u64(mode, failures as u64);
    print(mode, ", unexpectedly loaded: ");
    print_u64(mode, unexpected_ok as u64);
    println(mode, "");

    if after.bumped == before.bumped {
        println(
            mode,
            "  Confirmed: bump cursor unchanged -- every failed spawn's frames were reclaimed and reused, no leak.",
        );
    } else {
        print(mode, "  WARNING: bump cursor advanced by ");
        print_u64(mode, (after.bumped - before.bumped) as u64);
        println(mode, " fresh frames -- possible leak.");
    }
}

/// `reap <count>` -- the Phase 5 process-lifecycle-cleanup proof
/// (Milestone 1). Spawns and waits for `count` short-lived `hello`
/// processes back to back, each one left `Terminated` but not yet reclaimed
/// (the normal grace-period behavior -- see `task::REAP_GRACE_TICKS`), then
/// force-reaps everything at once (`task::reap_now`) and compares both the
/// live task count and the frame allocator's bump cursor before/after: if
/// terminated TCBs and their address spaces are genuinely reclaimed rather
/// than leaking, the task count returns to its starting value and the bump
/// cursor -- the one metric immune to frame reuse, see `spawnfail`'s docs
/// for why -- stops growing once the free list holds enough recycled frames
/// to cover one process's footprint.
fn handle_reap(mode: ConsoleMode, args: &str) {
    let count: u32 = match args.trim().parse() {
        Ok(n) if n > 0 => n,
        _ => {
            println(mode, "Usage: reap <count>");
            return;
        }
    };

    let Some(bytes) = embedded_program("hello") else {
        println(mode, "embedded 'hello' program missing");
        return;
    };

    // Warm-up: one full spawn/wait/reap cycle before measurement starts --
    // see `handle_spawnfail`'s docs for why an unwarmed bump cursor always
    // looks like growth on the very first iteration.
    if let Ok(id) = task::spawn_user_process("reap-warmup", bytes) {
        wait_for_terminated(id);
    }
    task::reap_now();

    let tasks_before = task::task_count();
    let frames_before = paging::frame_stats().map(|s| s.bumped).unwrap_or(0);

    for i in 0..count {
        let name = format!("reap-{}", i);
        match task::spawn_user_process(&name, bytes) {
            Ok(id) => wait_for_terminated(id),
            Err(reason) => {
                print(mode, "Process load error: ");
                println(mode, reason);
                return;
            }
        }
    }

    let tasks_pre_reap = task::task_count();
    task::reap_now();
    let tasks_after = task::task_count();
    let frames_after = paging::frame_stats().map(|s| s.bumped).unwrap_or(0);

    print(mode, "Live tasks before: ");
    print_u64(mode, tasks_before as u64);
    println(mode, "");
    print(mode, "Live tasks after ");
    print_u64(mode, count as u64);
    print(
        mode,
        " spawn/exit cycles (pre-reap, grace period still held): ",
    );
    print_u64(mode, tasks_pre_reap as u64);
    println(mode, "");
    print(mode, "Live tasks after forced reap: ");
    print_u64(mode, tasks_after as u64);
    println(mode, "");
    print(mode, "Frame bump cursor before: ");
    print_u64(mode, frames_before as u64);
    println(mode, "");
    print(mode, "Frame bump cursor after: ");
    print_u64(mode, frames_after as u64);
    println(mode, "");

    if tasks_after == tasks_before {
        println(
            mode,
            "  Confirmed: task count returned to baseline -- no leaked TCBs.",
        );
    } else {
        println(
            mode,
            "  WARNING: task count did not return to baseline -- possible TCB leak.",
        );
    }
    if frames_after == frames_before {
        println(
            mode,
            "  Confirmed: frame bump cursor unchanged -- address spaces fully reclaimed and reused.",
        );
    } else {
        print(mode, "  WARNING: frame bump cursor advanced by ");
        print_u64(mode, frames_after.saturating_sub(frames_before) as u64);
        println(mode, " fresh frames -- possible leak.");
    }
}

fn parse_positive_count(mode: ConsoleMode, args: &str, usage: &str) -> Option<u32> {
    match args.trim().parse() {
        Ok(count) if (1..=100).contains(&count) => Some(count),
        _ => {
            println(mode, usage);
            None
        }
    }
}

/// Prove the scheduler's ordinary grace-period path removes terminated TCBs
/// without a diagnostic force-reap call.
fn handle_automatic_reap(mode: ConsoleMode, args: &str) {
    let Some(count) = parse_positive_count(mode, args, "Usage: autoreap <1-100>") else {
        return;
    };
    let Some(bytes) = embedded_program("hello") else {
        println(mode, "autoreap: embedded hello missing");
        return;
    };
    task::reap_now();
    let before = desktop_lifecycle_stats();
    for index in 0..count {
        let name = format!("autoreap-{}", index);
        let Ok(id) = task::spawn_user_process(&name, bytes) else {
            println(mode, "automatic reap: FAIL spawn error");
            return;
        };
        wait_for_terminated(id);
    }
    let held = task::task_count();
    let started = interrupts::ticks();
    while interrupts::ticks().saturating_sub(started) < 510 {
        task::yield_now();
    }
    // One additional scheduling decision runs `prepare_switch` after the
    // grace deadline even if the tick boundary coincided with this task.
    task::yield_now();
    let after = desktop_lifecycle_stats();
    let passed = held >= before.tasks.saturating_add(count as usize)
        && after.tasks == before.tasks
        && after.live_frames == before.live_frames
        && after.heap_used == before.heap_used;
    println(
        mode,
        &format!(
            "automatic reap: {} count={} tasks_before={} held={} tasks_after={} live_frames_before={} live_frames_after={} heap_used_before={} heap_used_after={}",
            if passed { "PASS" } else { "FAIL" },
            count,
            before.tasks,
            held,
            after.tasks,
            before.live_frames,
            after.live_frames,
            before.heap_used,
            after.heap_used
        ),
    );
}

/// Exercise explicit kill of a never-scheduled user process, immediate
/// address-space reclamation, TCB destruction, and kernel-stack heap reuse.
fn handle_kill_reap(mode: ConsoleMode, args: &str) {
    let Some(count) = parse_positive_count(mode, args, "Usage: killreap <1-100>") else {
        return;
    };
    let Some(bytes) = embedded_program("hello") else {
        println(mode, "killreap: embedded hello missing");
        return;
    };

    let run_one = || -> Result<(), &'static str> {
        let id = task::spawn_user_process("kill-reap", bytes)?;
        task::kill(id)?;
        task::reap_now();
        Ok(())
    };
    task::reap_now();
    if run_one().is_err() {
        println(mode, "kill reap: FAIL warmup");
        return;
    }
    let before = desktop_lifecycle_stats();
    for _ in 0..count {
        if run_one().is_err() {
            println(mode, "kill reap: FAIL kill/reap operation");
            return;
        }
    }
    let after = desktop_lifecycle_stats();
    let passed = before.tasks == after.tasks
        && before.frame_bump == after.frame_bump
        && before.live_frames == after.live_frames
        && before.heap_used == after.heap_used;
    println(
        mode,
        &format!(
            "kill reap: {} count={} tasks_before={} tasks_after={} frame_bump_before={} frame_bump_after={} live_frames_before={} live_frames_after={} heap_used_before={} heap_used_after={}",
            if passed { "PASS" } else { "FAIL" },
            count,
            before.tasks,
            after.tasks,
            before.frame_bump,
            after.frame_bump,
            before.live_frames,
            after.live_frames,
            before.heap_used,
            after.heap_used
        ),
    );
}

/// Launch the packaged desktop from TuwaiqFS. Exit codes 10 and 11 are explicit
/// desktop requests to hand foreground ownership to File Manager or Terminal;
/// after that application exits, the shell relaunches `/apps/desktop` from the
/// filesystem. Escape returns to the shell.
fn handle_desktop(mode: ConsoleMode) {
    task::reap_now();
    print_desktop_lifecycle_stats("before");
    let mut next = "/apps/desktop";
    loop {
        match run_packaged_foreground(next) {
            Ok((id, exit)) => {
                clear_screen(mode);
                print(mode, "Filesystem application exited: ");
                print_process_result(mode, id);
                task::reap_now();
                match (next, exit) {
                    ("/apps/desktop", Some(10)) => next = "/apps/file-manager",
                    ("/apps/desktop", Some(11)) => next = "/apps/terminal",
                    ("/apps/file-manager" | "/apps/terminal", Some(0)) => next = "/apps/desktop",
                    ("/apps/desktop", Some(0)) => break,
                    _ => {
                        println(mode, "Desktop session stopped after application failure.");
                        break;
                    }
                }
            }
            Err(reason) => {
                print(mode, "Filesystem application load error: ");
                println(mode, reason);
                print_desktop_lifecycle_stats("spawn-failed");
                return;
            }
        }
    }
    print_desktop_lifecycle_stats("after");
}

fn run_packaged_foreground(path: &str) -> Result<(u32, Option<i32>), &'static str> {
    let bytes = vfs::read_file("/", path)?;
    if bytes.is_empty() || bytes.len() > vfs::MAX_EXECUTABLE_SIZE {
        return Err("invalid packaged executable size");
    }
    let name = vfs::basename(path);
    let id = spawn_foreground_process(name, &bytes)?;
    crate::serial_println!("desktop: packaged launch path={} pid={}", path, id);
    wait_for_terminated(id);
    let exit = task::info(id).ok().and_then(|info| info.exit_code);
    let telemetry = release_desktop_foreground(id);
    emit_input_telemetry("packaged-app-release", telemetry);
    Ok((id, exit))
}

/// Run the desktop and an ordinary Ring 3 peer concurrently. Both are
/// spawned before the shell yields; only the desktop owns foreground input.
fn handle_desktop_peer(mode: ConsoleMode) {
    let (Some(desktop_bytes), Some(peer_bytes)) = (
        embedded_program("desktop"),
        embedded_program("desktop_peer"),
    ) else {
        println(mode, "desktoppeer: embedded program missing");
        return;
    };

    task::reap_now();
    print_desktop_lifecycle_stats("peer-before");
    let desktop_id = match spawn_foreground_process("desktop", desktop_bytes) {
        Ok(id) => id,
        Err(reason) => {
            print(mode, "Desktop load error: ");
            println(mode, reason);
            return;
        }
    };
    let peer_id = match task::spawn_user_process("desktop_peer", peer_bytes) {
        Ok(id) => id,
        Err(reason) => {
            let _ = task::kill(desktop_id);
            let _ = release_desktop_foreground(desktop_id);
            task::reap_now();
            print(mode, "Desktop peer load error: ");
            println(mode, reason);
            return;
        }
    };

    crate::serial_println!(
        "desktop: concurrent session desktop_pid={} peer_pid={}",
        desktop_id,
        peer_id
    );
    print(mode, "Launching desktop pid ");
    print_u64(mode, desktop_id as u64);
    print(mode, " with Ring 3 peer pid ");
    print_u64(mode, peer_id as u64);
    println(mode, " (press Esc to exit desktop)...");

    wait_for_terminated(desktop_id);
    let telemetry = release_desktop_foreground(desktop_id);
    emit_input_telemetry("desktop-peer-release", telemetry);
    clear_screen(mode);
    print(mode, "Desktop exited: ");
    print_process_result(mode, desktop_id);
    wait_for_terminated(peer_id);
    print(mode, "Concurrent peer exited: ");
    print_process_result(mode, peer_id);
    task::reap_now();
    print_desktop_lifecycle_stats("peer-after");
}

/// Run a live desktop while a hostile sibling executes `ud2`. The CPU fault
/// must terminate only the sibling; the desktop then receives a normal Escape
/// event and exits zero, proving both isolation and shell recovery.
fn handle_desktop_fault_peer(mode: ConsoleMode) {
    let (Some(desktop_bytes), Some(fault_bytes)) =
        (embedded_program("desktop"), embedded_program("bad_ud2"))
    else {
        println(mode, "desktopfaultpeer: embedded program missing");
        return;
    };
    task::reap_now();
    let Ok(desktop_id) = spawn_foreground_process("desktop", desktop_bytes) else {
        println(mode, "desktop fault peer: FAIL desktop spawn");
        return;
    };
    let Ok(fault_id) = task::spawn_user_process("desktop-fault-peer", fault_bytes) else {
        let _ = task::kill(desktop_id);
        let _ = release_desktop_foreground(desktop_id);
        task::reap_now();
        println(mode, "desktop fault peer: FAIL hostile peer spawn");
        return;
    };
    wait_for_terminated(fault_id);
    let peer_exit = task::info(fault_id).ok().and_then(|info| info.exit_code);
    let desktop_survived = matches!(
        task::info(desktop_id),
        Ok(info) if info.state != task::TaskState::Terminated
    );
    crate::input::push_key_event(KeyEvent::Escape);
    wait_for_terminated(desktop_id);
    let desktop_exit = task::info(desktop_id).ok().and_then(|info| info.exit_code);
    let telemetry = release_desktop_foreground(desktop_id);
    emit_input_telemetry("desktop-fault-peer-release", telemetry);
    let passed = peer_exit == Some(132) && desktop_survived && desktop_exit == Some(0);
    clear_screen(mode);
    println(
        mode,
        &format!(
            "desktop fault peer: {} peer_exit={:?} desktop_survived={} desktop_exit={:?}",
            if passed { "PASS" } else { "FAIL" },
            peer_exit,
            desktop_survived,
            desktop_exit
        ),
    );
    task::reap_now();
}

/// Force-terminate a running desktop and verify foreground ownership and all
/// resource baselines recover even though the normal userspace exit path did
/// not run.
fn handle_desktop_kill_test(mode: ConsoleMode) {
    let Some(bytes) = embedded_program("desktop") else {
        println(mode, "desktopkilltest: embedded desktop missing");
        return;
    };
    task::reap_now();
    let before = desktop_lifecycle_stats();
    let Ok(id) = spawn_foreground_process("desktop-kill-test", bytes) else {
        println(mode, "desktop kill: FAIL spawn");
        return;
    };
    let started = interrupts::ticks();
    while interrupts::ticks().saturating_sub(started) < 2 {
        task::yield_now();
    }
    let killed = task::kill(id).is_ok();
    let _ = release_desktop_foreground(id);
    task::reap_now();
    let after = desktop_lifecycle_stats();
    let passed = killed
        && crate::keyboard::foreground_process_id().is_none()
        && before.tasks == after.tasks
        && before.live_frames == after.live_frames
        && before.heap_used == after.heap_used;
    clear_screen(mode);
    println(
        mode,
        &format!(
            "desktop kill: {} tasks_before={} tasks_after={} live_frames_before={} live_frames_after={} heap_used_before={} heap_used_after={}",
            if passed { "PASS" } else { "FAIL" },
            before.tasks,
            after.tasks,
            before.live_frames,
            after.live_frames,
            before.heap_used,
            after.heap_used
        ),
    );
}

fn inject_mouse_to(x: i32, y: i32, buttons: u8) -> Result<(), &'static str> {
    for _ in 0..16 {
        let (current_x, current_y) = crate::mouse::cursor_position();
        let dx = (x - current_x).clamp(-255, 255);
        let dy = (y - current_y).clamp(-255, 255);
        if dx == 0 && dy == 0 {
            // A fresh desktop initializes its private cursor at screen
            // center. If the global decoder is already at the requested
            // target from a prior cycle, a zero-delta packet emits no move
            // event and button edges would be interpreted at that stale
            // private coordinate. An away-and-back pair traverses the real
            // packet decoder/queue and deterministically synchronizes it.
            crate::mouse::inject_screen_packet(1, 0, buttons)?;
            return crate::mouse::inject_screen_packet(-1, 0, buttons);
        }
        crate::mouse::inject_screen_packet(dx as i16, dy as i16, buttons)?;
    }
    let (current_x, current_y) = crate::mouse::cursor_position();
    crate::serial_println!(
        "desktop interaction: mouse target failed target=({}, {}) current=({}, {})",
        x,
        y,
        current_x,
        current_y
    );
    Err("mouse cursor did not converge on target")
}

/// Drive live Ring-3 desktop policy through the production PS/2 decoder,
/// foreground queue, and INPUT_POLL syscall. Userspace emits exact markers
/// only after each model transition actually occurs.
fn handle_desktop_interaction_test(mode: ConsoleMode) {
    let Some(bytes) = embedded_program("desktop") else {
        println(mode, "desktopinteraction: embedded desktop missing");
        return;
    };
    task::reap_now();
    if run_one_desktop_cycle(bytes).is_err() {
        println(mode, "desktop interaction: FAIL warmup lifecycle");
        task::reap_now();
        return;
    }
    task::reap_now();
    let before = desktop_lifecycle_stats();
    let presents_before = crate::display::present_telemetry().count;
    let Ok(id) = spawn_foreground_process("desktop-interaction", bytes) else {
        println(mode, "desktop interaction: FAIL spawn");
        return;
    };
    if !wait_for_desktop_present(id, presents_before, 500) {
        let _ = task::kill(id);
        let _ = release_desktop_foreground(id);
        println(mode, "desktop interaction: FAIL first present timeout");
        return;
    }
    let interaction = x86_64::instructions::interrupts::without_interrupts(|| {
        // Measure the ready desktop's controlled interaction path, not host
        // mouse packets that may have arrived while its mmap/first present
        // was still starting. This diagnostic owns the foreground session
        // and injects the complete sequence below, so discard that startup
        // residue and begin telemetry at the actual measurement boundary.
        crate::input::clear();
        crate::input::reset_telemetry();
        crate::mouse::reset_decoder_for_diagnostics();
        (|| -> Result<(), &'static str> {
            inject_mouse_to(150, 100, 0)?;
            crate::mouse::inject_screen_packet(0, 0, 1)?;
            inject_mouse_to(210, 140, 1)?;
            crate::mouse::inject_screen_packet(0, 0, 0)?;
            inject_mouse_to(510, 140, 0)?;
            crate::mouse::inject_screen_packet(0, 0, 1)?;
            crate::mouse::inject_screen_packet(0, 0, 0)?;
            inject_mouse_to(50, 10, 0)?;
            crate::mouse::inject_screen_packet(0, 0, 1)?;
            crate::mouse::inject_screen_packet(0, 0, 0)?;
            crate::mouse::inject_screen_packet(0, 0, 1)?;
            crate::mouse::inject_screen_packet(0, 0, 0)?;
            inject_mouse_to(190, 200, 0)?;
            crate::mouse::inject_screen_packet(0, 0, 1)?;
            crate::mouse::inject_screen_packet(0, 0, 0)?;
            Ok(())
        })()
    });
    crate::input::push_key_event(KeyEvent::Escape);
    wait_for_terminated(id);
    let exit = task::info(id).ok().and_then(|info| info.exit_code);
    let telemetry = release_desktop_foreground(id);
    emit_input_telemetry("desktop-interaction-release", telemetry);
    task::reap_now();
    let after = desktop_lifecycle_stats();
    let passed = interaction.is_ok()
        && exit == Some(0)
        && telemetry.dropped_events == 0
        && before.tasks == after.tasks
        && before.live_frames == after.live_frames
        && before.heap_used == after.heap_used;
    clear_screen(mode);
    println(
        mode,
        &format!(
            "desktop interaction: {} exit={:?} tasks_before={} tasks_after={} live_frames_before={} live_frames_after={} heap_used_before={} heap_used_after={}",
            if passed { "PASS" } else { "FAIL" },
            exit,
            before.tasks,
            after.tasks,
            before.live_frames,
            after.live_frames,
            before.heap_used,
            after.heap_used
        ),
    );
}

fn acquire_desktop_foreground(id: u32) {
    crate::keyboard::set_foreground_process_id(Some(id));
    crate::serial_println!("input: foreground acquire owner=desktop pid={}", id);
}

/// Make a newly created user process visible to the scheduler and bind input
/// ownership without an interruptible gap. Otherwise the 100 Hz timer could
/// run the process between spawn and handoff, causing its first INPUT_POLL to
/// observe the shell as owner.
fn spawn_foreground_process(name: &str, bytes: &[u8]) -> Result<u32, &'static str> {
    let id = task::spawn_user_process_suspended(name, bytes, "/")?;
    let activated = x86_64::instructions::interrupts::without_interrupts(|| {
        acquire_desktop_foreground(id);
        task::activate_task(id)
    });
    if !activated {
        let _ = release_desktop_foreground(id);
        let _ = task::kill(id);
        task::reap_now();
        return Err("foreground activation failed");
    }
    Ok(id)
}

fn release_desktop_foreground(id: u32) -> crate::input::InputTelemetry {
    let telemetry = x86_64::instructions::interrupts::without_interrupts(|| {
        let telemetry = crate::input::telemetry();
        crate::keyboard::set_foreground_process_id(None);
        telemetry
    });
    crate::serial_println!("input: foreground release owner=shell former_pid={}", id);
    telemetry
}

fn emit_input_telemetry(label: &str, stats: crate::input::InputTelemetry) {
    let average_milli_ticks = if stats.delivered_mouse_moves == 0 {
        0
    } else {
        stats.mouse_age_total_ticks.saturating_mul(1000) / stats.delivered_mouse_moves
    };
    crate::serial_println!(
        "input: telemetry label={} depth={} max_depth={} coalesced_moves={} dropped={} mouse_delivered={} mouse_age_total_ticks={} mouse_age_max_ticks={} mouse_age_avg_milli_ticks={}",
        label,
        stats.current_depth,
        stats.max_depth,
        stats.coalesced_mouse_moves,
        stats.dropped_events,
        stats.delivered_mouse_moves,
        stats.mouse_age_total_ticks,
        stats.mouse_age_max_ticks,
        average_milli_ticks
    );
}

fn print_input_telemetry(mode: ConsoleMode, label: &str) {
    emit_input_telemetry(label, crate::input::telemetry());
    let mouse = crate::mouse::telemetry();
    crate::serial_println!(
        "mouse: telemetry initialized={} raw_bytes={} packets={} desync_bytes={} overflow_packets={}",
        crate::mouse::is_initialized(),
        mouse.raw_bytes,
        mouse.complete_packets,
        mouse.desync_bytes,
        mouse.overflow_packets
    );
    let present = crate::display::present_telemetry();
    let average_cycles = if present.count == 0 {
        0
    } else {
        present.total_cycles / present.count
    };
    let cycles_per_tick = crate::interrupts::average_tsc_cycles_per_tick().unwrap_or(0);
    let max_milli_ticks = if cycles_per_tick == 0 {
        0
    } else {
        present.max_cycles.saturating_mul(1000) / cycles_per_tick
    };
    crate::serial_println!(
        "display: telemetry presents={} total_cycles={} average_cycles={} max_cycles={} cycles_per_tick={} max_milli_ticks={}",
        present.count,
        present.total_cycles,
        average_cycles,
        present.max_cycles,
        cycles_per_tick,
        max_milli_ticks
    );
    println(mode, "Input telemetry emitted to serial.");
}

fn print_desktop_lifecycle_stats(label: &str) {
    let stats = desktop_lifecycle_stats();
    crate::serial_println!(
        "desktop: lifecycle label={} tasks={} frame_bump={} frame_current={} frame_free={} heap_used={}",
        label,
        stats.tasks,
        stats.frame_bump,
        stats.live_frames,
        stats.frame_free,
        stats.heap_used
    );
}

#[derive(Clone, Copy)]
struct DesktopLifecycleStats {
    tasks: usize,
    frame_bump: usize,
    live_frames: usize,
    frame_free: usize,
    heap_used: usize,
}

fn desktop_lifecycle_stats() -> DesktopLifecycleStats {
    let (frame_bump, frame_free) = paging::frame_stats()
        .map(|stats| (stats.bumped, stats.free_in_pool))
        .unwrap_or((0, 0));
    DesktopLifecycleStats {
        tasks: task::task_count(),
        frame_bump,
        live_frames: frame_bump.saturating_sub(frame_free),
        frame_free,
        heap_used: allocator::used(),
    }
}

/// Deterministic normal-exit/restart stress proof. One warm-up session grows
/// reusable allocator pools; the measured sessions must then return TCBs,
/// address spaces, kernel stacks, and mmap frames to the same baseline.
fn handle_desktop_cycle(mode: ConsoleMode, args: &str) {
    let cycles: u32 = match args.trim().parse() {
        Ok(count) if (1..=100).contains(&count) => count,
        _ => {
            println(mode, "Usage: desktopcycle <1-100>");
            return;
        }
    };
    let Some(bytes) = embedded_program("desktop") else {
        println(mode, "desktopcycle: embedded desktop missing");
        return;
    };

    task::reap_now();
    if let Err(reason) = run_one_desktop_cycle(bytes) {
        clear_screen(mode);
        println(
            mode,
            &format!("desktop lifecycle: FAIL warmup reason={}", reason),
        );
        task::reap_now();
        return;
    }
    task::reap_now();
    let before = desktop_lifecycle_stats();

    for cycle in 1..=cycles {
        if let Err(reason) = run_one_desktop_cycle(bytes) {
            clear_screen(mode);
            println(
                mode,
                &format!("desktop lifecycle: FAIL cycle={} reason={}", cycle, reason),
            );
            task::reap_now();
            return;
        }
        task::reap_now();
    }

    let after = desktop_lifecycle_stats();
    let passed = before.tasks == after.tasks
        && before.live_frames == after.live_frames
        && before.frame_bump == after.frame_bump
        && before.heap_used == after.heap_used;
    clear_screen(mode);
    println(
        mode,
        &format!(
            "desktop lifecycle: {} cycles={}",
            if passed { "PASS" } else { "FAIL" },
            cycles
        ),
    );
    println(
        mode,
        &format!(
            "desktop lifecycle: tasks before={} after={}",
            before.tasks, after.tasks
        ),
    );
    println(
        mode,
        &format!(
            "desktop lifecycle: live_frames before={} after={}",
            before.live_frames, after.live_frames
        ),
    );
    println(
        mode,
        &format!(
            "desktop lifecycle: frame_bump before={} after={}",
            before.frame_bump, after.frame_bump
        ),
    );
    println(
        mode,
        &format!(
            "desktop lifecycle: heap_used before={} after={}",
            before.heap_used, after.heap_used
        ),
    );
}

fn run_one_desktop_cycle(bytes: &'static [u8]) -> Result<(), &'static str> {
    let presents_before = crate::display::present_telemetry().count;
    let id = spawn_foreground_process("desktop-cycle", bytes)?;

    // Require a real successful present before requesting exit. A fixed sleep
    // could expire while a preemptible backbuffer allocation is still in
    // progress and accidentally test spawn->exit without ever running the UI.
    if !wait_for_desktop_present(id, presents_before, 500) {
        let _ = task::kill(id);
        let _ = release_desktop_foreground(id);
        return Err("desktop first present timed out");
    }
    crate::input::push_key_event(KeyEvent::Escape);

    let exit_wait_started = interrupts::ticks();
    let timed_out = loop {
        match task::info(id) {
            Ok(info) if info.state == task::TaskState::Terminated => break false,
            Err(_) => break true,
            _ if interrupts::ticks().saturating_sub(exit_wait_started) >= 500 => {
                let _ = task::kill(id);
                break true;
            }
            _ => task::yield_now(),
        }
    };

    let _telemetry = release_desktop_foreground(id);
    if timed_out {
        return Err("desktop exit timed out");
    }
    match task::info(id) {
        Ok(info) if info.exit_code == Some(0) => Ok(()),
        Ok(_) => Err("desktop did not exit with code 0"),
        Err(_) => Err("desktop TCB disappeared before verification"),
    }
}

fn wait_for_desktop_present(id: u32, before: u64, timeout_ticks: u64) -> bool {
    let started = interrupts::ticks();
    loop {
        if crate::display::present_telemetry().count > before {
            return true;
        }
        match task::info(id) {
            Ok(info) if info.state == task::TaskState::Terminated => return false,
            Err(_) => return false,
            _ if interrupts::ticks().saturating_sub(started) >= timeout_ticks => return false,
            _ => task::yield_now(),
        }
    }
}

fn handle_desktop_test(mode: ConsoleMode) {
    match desktop_window_model::self_test() {
        Ok(()) => println(mode, "desktoptest: PASS drag close z-order slot-reuse"),
        Err(reason) => {
            print(mode, "desktoptest: FAIL ");
            println(mode, reason);
        }
    }
}

fn handle_mouse_test(mode: ConsoleMode) {
    match crate::mouse::self_test() {
        Ok(()) => println(
            mode,
            "mousetest: PASS resync signed-motion y-inversion clamp overflow button-edges",
        ),
        Err(reason) => {
            print(mode, "mousetest: FAIL ");
            println(mode, reason);
        }
    }
}

fn handle_cd(mode: ConsoleMode, args: &str) {
    let path = args.trim();
    if path.is_empty() {
        println(mode, "Usage: cd <path>");
        return;
    }
    match vfs::shell_chdir(path) {
        Ok(_) => {}
        Err(reason) => print_fs_error(mode, reason),
    }
}

fn handle_touch(mode: ConsoleMode, args: &str) {
    let name = args.trim();
    if name.is_empty() {
        println(mode, "Usage: touch <name>");
        return;
    }
    match vfs::shell_create_file(name) {
        Ok(()) => {
            print(mode, "Created file: ");
            println(mode, name);
        }
        Err(reason) => print_fs_error(mode, reason),
    }
}

fn handle_mkdir(mode: ConsoleMode, args: &str) {
    let name = args.trim();
    if name.is_empty() {
        println(mode, "Usage: mkdir <name>");
        return;
    }
    match vfs::shell_create_dir(name) {
        Ok(()) => {
            print(mode, "Created directory: ");
            println(mode, name);
        }
        Err(reason) => print_fs_error(mode, reason),
    }
}

fn handle_cat(mode: ConsoleMode, args: &str) {
    let name = args.trim();
    if name.is_empty() {
        println(mode, "Usage: cat <name>");
        return;
    }
    match vfs::shell_read(name) {
        Ok(content) => println(mode, &content),
        Err(reason) => print_fs_error(mode, reason),
    }
}

fn handle_write(mode: ConsoleMode, args: &str) {
    let Some((name, text)) = split_first_token(args) else {
        println(mode, "Usage: write <name> <text>");
        return;
    };
    match vfs::shell_write(name, text) {
        Ok(()) => {
            print(mode, "Wrote to: ");
            println(mode, name);
        }
        Err(reason) => print_fs_error(mode, reason),
    }
}

fn handle_ps(mode: ConsoleMode) {
    match task::list() {
        Ok(tasks) => {
            println(mode, "PID   NAME             PRIV    STATE      EXIT");
            for task in tasks {
                print_u64(mode, task.id as u64);
                print(mode, "     ");
                pad_print(mode, &task.name, 17);
                pad_print(mode, task::privilege_label(task.privilege), 8);
                pad_print(mode, task::state_label(task.state), 11);
                match task.exit_code {
                    Some(code) => print_i64(mode, code as i64),
                    None => print(mode, "-"),
                }
                println(mode, "");
            }
        }
        Err(reason) => {
            print(mode, "Task error: ");
            println(mode, reason);
        }
    }
}

/// Print `text` left-padded to at least `width` columns with spaces --
/// keeps `ps`'s columns aligned regardless of name/state string length.
fn pad_print(mode: ConsoleMode, text: &str, width: usize) {
    print(mode, text);
    for _ in text.len()..width {
        print_char(mode, b' ');
    }
}

fn handle_taskinfo(mode: ConsoleMode, args: &str) {
    let args = args.trim();
    if args.is_empty() {
        match task::list() {
            Ok(tasks) => {
                for task in tasks {
                    print(mode, "Task ");
                    print_u64(mode, task.id as u64);
                    print(mode, ": ");
                    println(mode, &task.name);
                    print(mode, "  Privilege: ");
                    println(mode, task::privilege_label(task.privilege));
                    print(mode, "  State: ");
                    println(mode, task::state_label(task.state));
                }
            }
            Err(reason) => {
                print(mode, "Task error: ");
                println(mode, reason);
            }
        }
        return;
    }

    let id = parse_u32(args).unwrap_or(0);
    if id == 0 {
        println(mode, "Usage: taskinfo [id]");
        return;
    }

    match task::info(id) {
        Ok(task) => {
            print(mode, "PID: ");
            print_u64(mode, task.id as u64);
            println(mode, "");
            print(mode, "Name: ");
            println(mode, &task.name);
            print(mode, "Privilege: ");
            println(mode, task::privilege_label(task.privilege));
            print(mode, "State: ");
            println(mode, task::state_label(task.state));
            if task.privilege == task::Privilege::User {
                print(mode, "Exit code: ");
                match task.exit_code {
                    Some(code) => {
                        print_i64(mode, code as i64);
                        println(mode, "");
                    }
                    None => println(mode, "none (still running)"),
                }
                if let Some(pml4) = task::process_pml4_phys(id) {
                    print(mode, "Address space PML4: ");
                    print_hex(mode, pml4);
                    println(mode, "");
                }
                if let Some(frames) = task::process_frame_count(id) {
                    print(mode, "Address space frames: ");
                    print_u64(mode, frames as u64);
                    println(mode, "");
                }
            }
        }
        Err(reason) => {
            print(mode, "Task error: ");
            println(mode, reason);
        }
    }
}

fn handle_kill(mode: ConsoleMode, args: &str) {
    let args = args.trim();
    if args.is_empty() {
        println(mode, "Usage: kill <id>");
        return;
    }
    let id = parse_u32(args).unwrap_or(0);
    if id == 0 {
        println(mode, "Usage: kill <id>");
        return;
    }
    match task::kill(id) {
        Ok(()) => {
            print(mode, "Stopped task ");
            print_u64(mode, id as u64);
            println(mode, "");
        }
        Err(reason) => {
            print(mode, "Task error: ");
            println(mode, reason);
        }
    }
}

fn handle_net_command(mode: ConsoleMode, args: &str) {
    let sub = args.trim();
    if sub.eq_ignore_ascii_case("status") || sub.is_empty() {
        for line in net::status_lines() {
            println(mode, &line);
        }
        return;
    }
    if sub.eq_ignore_ascii_case("dhcp") {
        match net::configure_dhcp() {
            Ok(status) => {
                print(mode, "DHCP: configured ");
                if let Some(address) = status.address {
                    let line = alloc::format!("{}", address);
                    println(mode, &line);
                } else {
                    println(mode, "without an IPv4 address");
                }
            }
            Err(reason) => {
                print(mode, "DHCP error: ");
                println(mode, reason);
            }
        }
        return;
    }
    if let Some(name) = sub.strip_prefix("dns ") {
        match net::resolve_ipv4(name.trim()) {
            Ok(address) => {
                let line = alloc::format!("DNS: {} -> {}", name.trim(), address);
                println(mode, &line);
            }
            Err(reason) => {
                print(mode, "DNS error: ");
                println(mode, reason);
            }
        }
        return;
    }
    if sub.eq_ignore_ascii_case("shutdown-test") {
        match net::shutdown_hardware() {
            Ok(()) => println(
                mode,
                "net: PASS failure-cleanup device-reset DMA-scrub ownership-released",
            ),
            Err(reason) => {
                print(mode, "Network shutdown error: ");
                println(mode, reason);
            }
        }
        return;
    }
    print(mode, "Unknown net command: ");
    println(mode, sub);
}

fn handle_ping(mode: ConsoleMode, args: &str) {
    let host = args.trim();
    if host.is_empty() {
        println(mode, "Usage: ping <host>");
        return;
    }
    match net::ping(host) {
        Ok(message) => println(mode, &message),
        Err(reason) => {
            print(mode, "Network error: ");
            println(mode, reason);
        }
    }
}

fn handle_ai_command(mode: ConsoleMode, line: &str, args: &str) {
    let sub = args.trim();

    if sub.eq_ignore_ascii_case("status") {
        let status = ai_bridge::bridge().status();
        print(mode, "AI Bridge: ");
        println(mode, if status.online { "online" } else { "offline" });
        print(mode, "Mode: ");
        match status.mode {
            ai_bridge::BridgeMode::Stub => println(mode, "stub"),
        }
        print(mode, "Phase: ");
        print_u64(mode, status.phase as u64);
        println(mode, "");
        return;
    }

    if sub.eq_ignore_ascii_case("help") {
        for line in ai_bridge::bridge().help_lines() {
            println(mode, line);
        }
        return;
    }

    if sub.is_empty() {
        println(mode, ai_bridge::bridge().offline_notice());
        println(mode, "Try: ai help | ai status | ask <question>");
        return;
    }

    print(mode, "Unknown command: ");
    println(mode, line);
}

fn handle_ask_command(mode: ConsoleMode, question: &str) {
    let response = ai_bridge::bridge().ask(question.trim());
    for line in response.lines() {
        println(mode, line);
    }
}

fn print_uptime(mode: ConsoleMode) {
    let seconds = interrupts::uptime_seconds();
    print(mode, "Uptime: ");
    print_u64(mode, seconds);
    print(mode, " s (");
    print_u64(mode, interrupts::ticks());
    println(mode, " timer ticks)");
}

fn print_fs_error(mode: ConsoleMode, reason: &str) {
    print(mode, "Filesystem error: ");
    println(mode, reason);
}

fn print_help(mode: ConsoleMode) {
    println(mode, "Commands:");
    println(
        mode,
        "  help | about | version | banner | sysinfo | monitor",
    );
    println(mode, "  uptime | reboot | clear | cls | echo <text>");
    println(mode, "  meminfo | memtest");
    println(
        mode,
        "  ls [path] | pwd | cd <path> | mounts | touch | mkdir | cat | write",
    );
    println(
        mode,
        "  ps | taskinfo | kill | yield | net status | net dhcp | net dns <name> | ping",
    );
    println(mode, "  run <program> | notes | editor");
    println(
        mode,
        "  runelf <hello|bad_syscall|bad_pointer|bad_privileged|bad_kernel|bad_unmapped|bad_ud2|",
    );
    println(
        mode,
        "         bad_divzero|bad_mmap|bad_munmap|bad_display|bad_input|bad_net|mmap_ro_fault|",
    );
    println(
        mode,
        "         mmap_nx_fault|post_unmap_fault|mmap_exhaustion|mmap_partial_failure|desktop|desktop_peer>",
    );
    println(
        mode,
        "  installapp <name> | runfs <path> | vfstest | storagetest",
    );
    println(mode, "  fsinterrupttest | fsexhausttest | nvmetest");
    println(mode, "  aipreviewtest | desktopaitest");
    println(mode, "  isolate [bad_program]");
    println(mode, "  spawnfail <count>");
    println(mode, "  reap <count>");
    println(mode, "  autoreap <count> | killreap <count>");
    println(
        mode,
        "  desktop | desktoppeer | desktopfaultpeer | desktopkilltest | desktopinteraction",
    );
    println(
        mode,
        "  desktopcycle <count> | desktoptest | mousetest | desktopstats | inputstats",
    );
    println(mode, "  ai | ai status | ask <question>");
    println(mode, "");
    println(mode, "Tip: use Up/Down for history, Tab to complete.");
}

fn print_banner(mode: ConsoleMode) {
    println(mode, "========================================");
    println(mode, "  TuwaiqOS v0.5");
    println(mode, "  AI-Native Experimental OS");
    println(mode, "========================================");
}

fn print_sysinfo(boot_info: &BootInfo, mode: ConsoleMode) {
    let mut usable_bytes: u64 = 0;
    for region in boot_info.memory_regions.iter() {
        if region.kind == MemoryRegionKind::Usable {
            usable_bytes = usable_bytes.saturating_add(region.end.saturating_sub(region.start));
        }
    }

    println(mode, "System Information");
    println(mode, "  OS: TuwaiqOS v0.5");
    println(mode, "  Architecture: x86_64");
    print(mode, "  Uptime: ");
    print_u64(mode, interrupts::uptime_seconds());
    println(mode, " s");
    print(mode, "  Usable RAM: ");
    print_u64(mode, usable_bytes);
    println(mode, " bytes");
    print(mode, "  Kernel heap: ");
    print_u64(mode, memory::HEAP_SIZE as u64);
    print(mode, " bytes (");
    print_u64(mode, allocator::used() as u64);
    print(mode, " used, ");
    print_u64(mode, allocator::free() as u64);
    println(mode, " free)");
    print(mode, "  Paging: ");
    if paging::is_active() {
        print(mode, "active");
        if let Some(stats) = paging::frame_stats() {
            print(mode, " (");
            print_u64(mode, stats.allocated as u64);
            print(mode, " frames allocated, ");
            print_u64(mode, stats.free_in_pool as u64);
            print(mode, " in free pool)");
        }
        println(mode, "");
    } else {
        println(mode, "inactive (static-array heap fallback)");
    }
    print(mode, "  Filesystem: ");
    println(mode, crate::fs::label());
    println(
        mode,
        "  Tasks: preemptive scheduler (round-robin, 50ms slices)",
    );
    println(mode, "  Network: loopback");
    println(mode, "  AI Bridge: offline (stub)");
    if let Some(fb) = boot_info.framebuffer.as_ref() {
        let info = fb.info();
        print(mode, "  Framebuffer: ");
        print_u64(mode, info.width as u64);
        print(mode, "x");
        print_u64(mode, info.height as u64);
        println(mode, "");
    } else {
        println(mode, "  Display: VGA text mode");
    }
}

fn command_names() -> &'static [&'static str] {
    &[
        "help",
        "about",
        "version",
        "banner",
        "sysinfo",
        "monitor",
        "uptime",
        "reboot",
        "clear",
        "cls",
        "echo",
        "meminfo",
        "memtest",
        "ls",
        "mounts",
        "pwd",
        "cd",
        "touch",
        "mkdir",
        "cat",
        "write",
        "ps",
        "taskinfo",
        "kill",
        "yield",
        "net",
        "ping",
        "run",
        "notes",
        "editor",
        "runelf",
        "runfs",
        "installapp",
        "vfstest",
        "storagetest",
        "nvmetest",
        "fsinterrupttest",
        "fsexhausttest",
        "aipreviewtest",
        "desktopaitest",
        "isolate",
        "spawnfail",
        "reap",
        "autoreap",
        "killreap",
        "desktop",
        "desktoppeer",
        "desktopfaultpeer",
        "desktopkilltest",
        "desktopinteraction",
        "desktopcycle",
        "desktoptest",
        "mousetest",
        "desktopstats",
        "inputstats",
        "ai",
        "ask",
    ]
}

fn split_command(line: &str) -> (&str, &str) {
    let line = line.trim();
    match line.find(char::is_whitespace) {
        Some(index) => {
            let (command, rest) = line.split_at(index);
            (command.trim(), rest.trim())
        }
        None => (line, ""),
    }
}

fn split_first_token(args: &str) -> Option<(&str, &str)> {
    let args = args.trim();
    if args.is_empty() {
        return None;
    }
    match args.find(char::is_whitespace) {
        Some(index) => {
            let (first, rest) = args.split_at(index);
            Some((first.trim(), rest.trim()))
        }
        None => Some((args, "")),
    }
}

fn parse_u32(text: &str) -> Option<u32> {
    let mut value: u32 = 0;
    for ch in text.bytes() {
        if ch.is_ascii_digit() {
            value = value.saturating_mul(10).saturating_add((ch - b'0') as u32);
        } else {
            return None;
        }
    }
    Some(value)
}

fn print_meminfo(boot_info: &BootInfo, mode: ConsoleMode) {
    let regions = &boot_info.memory_regions;
    let mut usable_bytes: u64 = 0;

    println(mode, "Memory map:");
    for (index, region) in regions.iter().enumerate() {
        let size = region.end.saturating_sub(region.start);
        let kind = match region.kind {
            MemoryRegionKind::Usable => {
                usable_bytes = usable_bytes.saturating_add(size);
                "usable"
            }
            MemoryRegionKind::Bootloader => "bootloader",
            MemoryRegionKind::UnknownBios(_) => "bios",
            MemoryRegionKind::UnknownUefi(_) => "uefi",
            _ => "other",
        };

        print(mode, "  region ");
        print_u64(mode, index as u64);
        print(mode, ": ");
        print_hex(mode, region.start);
        print(mode, "-");
        print_hex(mode, region.end);
        print(mode, " (");
        print_u64(mode, size);
        print(mode, " bytes, ");
        print(mode, kind);
        println(mode, ")");
    }

    println(mode, "");
    print(mode, "Total usable RAM: ");
    print_u64(mode, usable_bytes);
    println(mode, " bytes");
}

fn print(mode: ConsoleMode, text: &str) {
    crate::serial_print!("{}", text);
    match mode {
        ConsoleMode::Framebuffer => crate::framebuffer_console::print(text),
        ConsoleMode::Vga => crate::vga_buffer::print(text),
        ConsoleMode::Serial => {}
    }
}

fn println(mode: ConsoleMode, text: &str) {
    crate::serial_println!("{}", text);
    match mode {
        ConsoleMode::Framebuffer => crate::framebuffer_console::println(text),
        ConsoleMode::Vga => crate::vga_buffer::println(text),
        ConsoleMode::Serial => {}
    }
}

fn print_char(mode: ConsoleMode, ch: u8) {
    print(mode, core::str::from_utf8(&[ch]).unwrap_or("?"));
}

fn backspace(mode: ConsoleMode) {
    crate::serial_print!("\x08 \x08");
    match mode {
        ConsoleMode::Framebuffer => crate::framebuffer_console::backspace(),
        ConsoleMode::Vga => crate::vga_buffer::backspace(),
        ConsoleMode::Serial => {}
    }
}

fn clear_screen(mode: ConsoleMode) {
    match mode {
        ConsoleMode::Framebuffer => crate::framebuffer_console::clear_screen(),
        ConsoleMode::Vga => crate::vga_buffer::clear_screen(),
        ConsoleMode::Serial => {}
    }
}

fn print_u64(mode: ConsoleMode, mut value: u64) {
    if value == 0 {
        print_char(mode, b'0');
        return;
    }
    let mut digits = [0u8; 20];
    let mut count = 0;
    while value > 0 {
        digits[count] = b'0' + (value % 10) as u8;
        value /= 10;
        count += 1;
    }
    while count > 0 {
        count -= 1;
        print_char(mode, digits[count]);
    }
}

fn print_i64(mode: ConsoleMode, value: i64) {
    if value < 0 {
        print_char(mode, b'-');
        // `wrapping_neg` rather than plain `-value`: avoids overflow for
        // `i64::MIN`, whose magnitude doesn't fit in an `i64` (this ABI
        // never produces anything near that extreme, but the conversion
        // should not panic even in principle).
        print_u64(mode, value.wrapping_neg() as u64);
    } else {
        print_u64(mode, value as u64);
    }
}

fn print_hex(mode: ConsoleMode, mut value: u64) {
    print(mode, "0x");
    if value == 0 {
        print_char(mode, b'0');
        return;
    }
    let mut digits = [0u8; 16];
    let mut count = 0;
    while value > 0 {
        let nibble = (value & 0xF) as u8;
        digits[count] = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + (nibble - 10)
        };
        value >>= 4;
        count += 1;
    }
    while count > 0 {
        count -= 1;
        print_char(mode, digits[count]);
    }
}
