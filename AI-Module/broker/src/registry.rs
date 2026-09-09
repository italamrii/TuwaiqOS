//! Tool Registry: the single place that maps a `tool` name string to (a)
//! whether it's a known tool at all, and (b) the function that executes it.
//!
//! This is intentionally a flat, exhaustive match, not a dynamic/plugin
//! lookup -- the whole point of Phase 1 is that the set of callable tools is
//! fixed at compile time and reviewable in one place.

use crate::protocol::ErrorCode;
use crate::tools;

pub fn dispatch(tool: &str, arguments: &serde_json::Value) -> tools::ToolResult {
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
        "get_network_status" => {
            require_empty_arguments(tool, arguments)?;
            tools::get_network_status(arguments)
        }
        "kill_process" => tools::kill_process(arguments),
        "close_application" => tools::close_application(arguments),
        "launch_application" => tools::launch_application(arguments),
        _ => Err((
            ErrorCode::UnknownTool,
            format!("'{tool}' is not a registered tool"),
        )),

    }
}

/// The five read-only tools take no arguments. Rejecting a non-empty object
/// here (rather than silently ignoring extra fields) keeps the contract
/// strict and catches a confused/buggy Python caller early.
fn require_empty_arguments(tool: &str, arguments: &serde_json::Value) -> Result<(), (ErrorCode, String)> {
    let is_empty_object = matches!(arguments, serde_json::Value::Object(map) if map.is_empty())
        || matches!(arguments, serde_json::Value::Null);
    if is_empty_object {
        Ok(())
    } else {
        Err((
            ErrorCode::InvalidArguments,
            format!("'{tool}' takes no arguments"),
        ))
    }
}
