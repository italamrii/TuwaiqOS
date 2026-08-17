//! Tool Registry: fixed tool set, schemas, risk classes, and dispatch.
//!
//! This is intentionally a flat, exhaustive match — not a dynamic plugin
//! lookup. The callable tool set is fixed at compile time and reviewable
//! here. Python may mirror schemas for UX/validation, but this module is
//! the enforcement authority.

use crate::protocol::{ErrorCode, RiskClass};
use crate::tools;
use serde_json::{json, Value};

pub struct ToolMeta {
    pub name: &'static str,
    pub risk: RiskClass,
    pub description: &'static str,
}

pub const TOOL_META: &[ToolMeta] = &[
    ToolMeta {
        name: "get_system_info",
        risk: RiskClass::Read,
        description: "Read host OS name, version, kernel, hostname, and uptime.",
    },
    ToolMeta {
        name: "get_cpu_info",
        risk: RiskClass::Read,
        description: "Read CPU model, core count, and usage percent.",
    },
    ToolMeta {
        name: "get_memory_info",
        risk: RiskClass::Read,
        description: "Read memory totals/usage and top memory consumers.",
    },
    ToolMeta {
        name: "get_disk_info",
        risk: RiskClass::Read,
        description: "Read mounted volume capacity and usage.",
    },
    ToolMeta {
        name: "list_processes",
        risk: RiskClass::Read,
        description: "List top processes by CPU/memory (bounded).",
    },
    ToolMeta {
        name: "launch_application",
        risk: RiskClass::LowRiskAction,
        description: "Launch an allowlisted application by fixed app_id only.",
    },
];

pub fn lookup_meta(tool: &str) -> Option<&'static ToolMeta> {
    TOOL_META.iter().find(|m| m.name == tool)
}

pub fn classify(tool: &str) -> RiskClass {
    if let Some(meta) = lookup_meta(tool) {
        return meta.risk;
    }
    match tool {
        "terminate_process" => RiskClass::SensitiveAction,
        "run_shell" | "execute_command" | "shell" | "bash" => RiskClass::Forbidden,
        _ => RiskClass::Forbidden,
    }
}

/// Policy gate applied before dispatch. Confirmation is never accepted from
/// the request body — only from an external agent confirmation state machine.
pub fn policy_decision(tool: &str) -> PolicyDecision {
    let risk = classify(tool);
    match risk {
        RiskClass::Read => PolicyDecision {
            allowed: true,
            risk,
            requires_confirmation: false,
            reason: "read-only telemetry",
        },
        RiskClass::LowRiskAction => PolicyDecision {
            allowed: true,
            risk,
            requires_confirmation: false,
            reason: "low-risk allowlisted action",
        },
        RiskClass::SensitiveAction => PolicyDecision {
            allowed: false,
            risk,
            requires_confirmation: true,
            reason: "sensitive action requires external confirmation and is not implemented in V1",
        },
        RiskClass::Forbidden => PolicyDecision {
            allowed: false,
            risk,
            requires_confirmation: false,
            reason: "tool is forbidden",
        },
    }
}

#[derive(Debug, Clone)]
pub struct PolicyDecision {
    pub allowed: bool,
    pub risk: RiskClass,
    pub requires_confirmation: bool,
    pub reason: &'static str,
}

pub fn tool_catalog_json() -> Value {
    let tools: Vec<Value> = TOOL_META
        .iter()
        .map(|m| {
            json!({
                "name": m.name,
                "risk": m.risk,
                "description": m.description,
            })
        })
        .collect();
    json!({ "tools": tools, "protocol_version": crate::protocol::PROTOCOL_VERSION })
}

pub fn dispatch(tool: &str, arguments: &Value) -> tools::ToolResult {
    let decision = policy_decision(tool);
    if !decision.allowed {
        let code = match decision.risk {
            RiskClass::SensitiveAction => ErrorCode::PermissionDenied,
            RiskClass::Forbidden if is_explicit_forbidden_name(tool) => ErrorCode::PermissionDenied,
            RiskClass::Forbidden => ErrorCode::UnknownTool,
            _ => ErrorCode::PermissionDenied,
        };
        return Err((code, decision.reason.to_string()));
    }

    match tool {
        "get_system_info" => {
            require_empty_arguments(tool, arguments)?;
            tools::get_system_info(arguments)
        }
        "get_cpu_info" => {
            require_empty_arguments(tool, arguments)?;
            tools::get_cpu_info(arguments)
        }
        "get_memory_info" => {
            require_empty_arguments(tool, arguments)?;
            tools::get_memory_info(arguments)
        }
        "get_disk_info" => {
            require_empty_arguments(tool, arguments)?;
            tools::get_disk_info(arguments)
        }
        "list_processes" => {
            require_empty_arguments(tool, arguments)?;
            tools::list_processes(arguments)
        }
        "launch_application" => tools::launch_application(arguments),
        _ => Err((
            ErrorCode::UnknownTool,
            format!("'{tool}' is not a registered tool"),
        )),
    }
}

fn is_explicit_forbidden_name(tool: &str) -> bool {
    matches!(
        tool,
        "run_shell" | "execute_command" | "shell" | "bash" | "terminate_process"
    )
}

fn require_empty_arguments(tool: &str, arguments: &Value) -> Result<(), (ErrorCode, String)> {
    let is_empty_object = matches!(arguments, Value::Object(map) if map.is_empty())
        || matches!(arguments, Value::Null);
    if is_empty_object {
        Ok(())
    } else {
        Err((
            ErrorCode::InvalidArguments,
            format!("'{tool}' takes no arguments"),
        ))
    }
}
