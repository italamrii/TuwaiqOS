//! The `launch_application` allowlist.
//!
//! This is the single most security-sensitive table in the broker. Python
//! never supplies a binary path or command line -- only a short `app_id`
//! string, which is looked up here. If it's not in this table, nothing is
//! launched, full stop. There is no fallback to PATH lookup, shell
//! execution, or fuzzy matching.
//!
//! Adding a new launchable application means adding a new entry here (a
//! code change reviewed like any other), not something Python or a running
//! agent can register or influence at runtime.

pub struct AppEntry {
    pub app_id: &'static str,
    /// Absolute path only, by design -- never resolved via $PATH, so a
    /// hostile or misconfigured PATH cannot redirect this to a different
    /// binary.
    pub binary_path: &'static str,
    /// Fixed argument list. Never extended with anything from the request.
    pub args: &'static [&'static str],
}

pub const ALLOWLIST: &[AppEntry] = &[
    AppEntry {
        app_id: "firefox",
        binary_path: "/usr/bin/firefox",
        args: &[],
    },
    AppEntry {
        app_id: "vscode",
        binary_path: "/usr/bin/code",
        args: &[],
    },
    AppEntry {
        app_id: "terminal",
        binary_path: "/usr/bin/x-terminal-emulator",
        args: &[],
    },
    AppEntry {
        app_id: "file_manager",
        binary_path: "/usr/bin/nautilus",
        args: &[],
    },
];

pub fn lookup(app_id: &str) -> Option<&'static AppEntry> {
    ALLOWLIST.iter().find(|entry| entry.app_id == app_id)
}
