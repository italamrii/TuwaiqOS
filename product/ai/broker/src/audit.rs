//! Append-only audit log. Every request that reaches the broker is logged
//! exactly once with its outcome — including malformed/unknown/denied
//! requests. Never logs secrets (passwords/tokens/api keys).

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

use once_cell_lite::Lazy;
use serde_json::{json, Map, Value};

use crate::registry::{self, PolicyDecision};

static LOG_PATH: &str = "tuwaiq-agent-broker-audit.log";

static LOG_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

pub fn record(
    request_id: &str,
    tool: &str,
    arguments: &Value,
    outcome: &str,
) {
    let decision = registry::policy_decision(tool);
    record_with_policy(request_id, tool, arguments, outcome, &decision, None);
}

pub fn record_with_policy(
    request_id: &str,
    tool: &str,
    arguments: &Value,
    outcome: &str,
    decision: &PolicyDecision,
    confirmation_required: Option<bool>,
) {
    let line = json!({
        "timestamp": crate::protocol::now_rfc3339(),
        "request_id": request_id,
        "tool": tool,
        "arguments": redact_arguments(arguments),
        "outcome": outcome,
        "policy": {
            "risk": decision.risk,
            "allowed": decision.allowed,
            "requires_confirmation": confirmation_required
                .unwrap_or(decision.requires_confirmation),
            "reason": decision.reason,
        },
    });

    let _guard = LOG_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let file = OpenOptions::new().create(true).append(true).open(LOG_PATH);
    match file {
        Ok(mut f) => {
            let _ = writeln!(f, "{line}");
        }
        Err(e) => {
            eprintln!("tuwaiq-agent-broker: WARNING failed to write audit log: {e}");
        }
    }
}

fn redact_arguments(arguments: &Value) -> Value {
    let Value::Object(map) = arguments else {
        return arguments.clone();
    };
    let mut out = Map::new();
    for (key, value) in map {
        let key_l = key.to_ascii_lowercase();
        if key_l.contains("password")
            || key_l.contains("token")
            || key_l.contains("secret")
            || key_l.contains("api_key")
            || key_l.contains("authorization")
        {
            out.insert(key.clone(), Value::String("[redacted]".to_string()));
        } else {
            out.insert(key.clone(), value.clone());
        }
    }
    Value::Object(out)
}

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
