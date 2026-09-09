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
//!
//! NOTE ON BINARY PATHS BELOW: these target KDE Plasma's standard default
//! applications (konsole, dolphin), matching TuwaiqOS's desktop
//! environment per project direction. **These exact paths are not yet
//! confirmed against a real TuwaiqOS/KDE installation** -- flagged with
//! `UNCONFIRMED` below. Whoever owns the desktop environment decision
//! should verify these paths (or the actual install locations, which can
//! differ by distro/packaging) before this is relied on in production.

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
    // UNCONFIRMED: KDE's default terminal emulator. Konsole ships at this
    // path on most distros' KDE packaging, but verify against the actual
    // TuwaiqOS image before relying on this.
    AppEntry {
        app_id: "terminal",
        binary_path: "/usr/bin/konsole",
        args: &[],
    },
    // UNCONFIRMED: KDE's default file manager (Dolphin), same caveat as
    // above -- replaces the earlier GNOME-oriented "nautilus" entry, which
    // does not match a KDE desktop.
    AppEntry {
        app_id: "file_manager",
        binary_path: "/usr/bin/dolphin",
        args: &[],
    },
    // UNCONFIRMED: KDE's default text editor.
    AppEntry {
        app_id: "text_editor",
        binary_path: "/usr/bin/kate",
        args: &[],
    },
    // UNCONFIRMED: KDE System Settings. Binary name has varied across
    // Plasma versions (systemsettings vs systemsettings5) -- confirm
    // against the actual Plasma version TuwaiqOS ships.
    AppEntry {
        app_id: "settings",
        binary_path: "/usr/bin/systemsettings",
        args: &[],
    },
];

pub fn lookup(app_id: &str) -> Option<&'static AppEntry> {
    ALLOWLIST.iter().find(|entry| entry.app_id == app_id)
}

/// Derives the expected running-process name from an allowlisted app's
/// binary filename, e.g. `/usr/bin/firefox` -> `"firefox"`. This is an
/// approximation: a real running process's reported name doesn't always
/// exactly match its binary's filename (packaging/platform differences),
/// which is why `close_application` in tools.rs matches case-insensitively
/// and treats this as a filter, not a guaranteed-unique lookup.
pub fn expected_process_name(app_id: &str) -> Option<&'static str> {
    lookup(app_id).map(|entry| {
        entry
            .binary_path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(entry.binary_path)
    })
}
