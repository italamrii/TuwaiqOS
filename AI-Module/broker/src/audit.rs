//! Append-only audit log. Every request that reaches the broker is logged
//! exactly once with its outcome -- including malformed/unknown/denied
//! requests, not just successful ones. This is what lets a later reviewer
//! answer "what did the AI actually try to do," independent of whatever the
//! Python side claims happened.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

use once_cell_lite::Lazy;

static LOG_PATH: &str = "tuwaiq-agent-broker-audit.log";

static LOG_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

pub fn record(
    request_id: &str,
    tool: &str,
    arguments: &serde_json::Value,
    outcome: &str,
) {
    let line = serde_json::json!({
        "timestamp": crate::protocol::now_rfc3339(),
        "request_id": request_id,
        "tool": tool,
        "arguments": arguments,
        "outcome": outcome,
    });

    let _guard = LOG_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let file = OpenOptions::new().create(true).append(true).open(LOG_PATH);
    match file {
        Ok(mut f) => {
            let _ = writeln!(f, "{line}");
        }
        Err(e) => {
            // Audit logging must never crash the broker or block a
            // response -- if the log file can't be written, note it on
            // stderr (visible to whoever runs the broker) and continue.
            eprintln!("tuwaiq-agent-broker: WARNING failed to write audit log: {e}");
        }
    }
}

/// Minimal `once_cell`-free lazy-static so this prototype has no extra
/// dependency for a single static Mutex. A real dependency add is fine for
/// the final version; kept inline here to keep the Cargo.toml short.
mod once_cell_lite {
    use std::sync::OnceLock;

    pub struct Lazy<T> {
        cell: OnceLock<T>,
        init: fn() -> T,
    }

    impl<T> Lazy<T> {
        pub const fn new(init: fn() -> T) -> Self {
            Self {
                cell: OnceLock::new(),
                init,
            }
        }
    }

    impl<T> std::ops::Deref for Lazy<T> {
        type Target = T;
        fn deref(&self) -> &T {
            self.cell.get_or_init(self.init)
        }
    }
}
