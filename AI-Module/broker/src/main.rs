//! tuwaiq-agent-broker -- the only process allowed to touch the OS on
//! behalf of the Tuwaiq AI Python agent.
//!
//! Phase 1 transport: newline-delimited JSON over stdin/stdout. Python
//! writes one `ToolRequest` JSON object per line to this process's stdin
//! and reads one `ToolResponse` JSON object per line from stdout. This
//! keeps the prototype simple (no socket/IPC plumbing yet) while already
//! enforcing the real protocol contract -- swapping the transport for a
//! Unix domain socket later does not change `registry.rs`, `tools.rs`, or
//! the protocol at all.
//!
//! Design invariant this file exists to guarantee: a crash or malformed
//! input from Python can never crash this process. Every error path
//! returns a `ToolResponse` with `status: error` and continues the loop.

mod allowlist;
mod audit;
mod procinfo;
mod protocol;
mod registry;
#[cfg(test)]
mod registry_tests;
mod tools;

use std::io::{self, BufRead, Write};

use protocol::{ErrorCode, ToolRequest, ToolResponse, PROTOCOL_VERSION};

fn main() {
    eprintln!("tuwaiq-agent-broker: starting (protocol v{PROTOCOL_VERSION})");
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("tuwaiq-agent-broker: stdin read error: {e}");
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let response = handle_line(&line);
        let serialized = serde_json::to_string(&response)
            .unwrap_or_else(|_| r#"{"status":"error","error":{"code":"internal_error","message":"failed to serialize response"}}"#.to_string());
        if writeln!(out, "{serialized}").is_err() {
            // Python's end of the pipe is gone -- nothing left to do but
            // stop. The broker itself is still alive and would happily
            // serve a fresh Python process reconnecting on stdin/stdout in
            // a real socket-based transport (see architecture.md).
            eprintln!("tuwaiq-agent-broker: stdout closed, exiting read loop");
            break;
        }
        let _ = out.flush();
    }
    eprintln!("tuwaiq-agent-broker: input closed, shutting down");
}

fn handle_line(line: &str) -> ToolResponse {
    let request: ToolRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            audit::record("unknown", "unknown", &serde_json::Value::Null, "malformed_request");
            return ToolResponse::malformed(&e.to_string());
        }
    };

    if request.protocol_version != PROTOCOL_VERSION {
        audit::record(
            &request.request_id,
            &request.tool,
            &request.arguments,
            "unsupported_protocol_version",
        );
        return ToolResponse::error(
            &request.request_id,
            ErrorCode::UnsupportedProtocolVersion,
            format!(
                "broker supports protocol {PROTOCOL_VERSION}, request used {}",
                request.protocol_version
            ),
        );
    }

    match registry::dispatch(&request.tool, &request.arguments) {
        Ok(result) => {
            audit::record(&request.request_id, &request.tool, &request.arguments, "ok");
            ToolResponse::ok(&request.request_id, result)
        }
        Err((code, message)) => {
            audit::record(
                &request.request_id,
                &request.tool,
                &request.arguments,
                &format!("error:{code:?}"),
            );
            ToolResponse::error(&request.request_id, code, message)
        }
    }
}
