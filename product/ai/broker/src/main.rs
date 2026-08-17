//! tuwaiq-agent-broker -- the only process allowed to touch the OS on
//! behalf of the Tuwaiq AI Python agent.
//!
//! Transports:
//! - Default: newline-delimited JSON over stdin/stdout (dev / tests).
//! - Product (Unix): `--unix-socket PATH` accepts local Unix-domain
//!   connections with the same NDJSON framing.
//!
//! Design invariant: a crash or malformed input from Python can never crash
//! this process. Every error path returns a `ToolResponse` with
//! `status: error` and continues the loop.

mod allowlist;
mod audit;
mod procinfo;
mod protocol;
mod registry;
#[cfg(test)]
mod registry_tests;
mod tools;

use std::env;
use std::io::{self, BufRead, Write};

use protocol::{ErrorCode, ToolRequest, ToolResponse, PROTOCOL_VERSION};

fn main() {
    let mut args = env::args().skip(1);
    let mut socket_path: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--unix-socket" => {
                socket_path = args.next().or_else(|| {
                    eprintln!("tuwaiq-agent-broker: --unix-socket requires a path");
                    None
                });
            }
            "--help" | "-h" => {
                eprintln!(
                    "usage: tuwaiq-agent-broker [--unix-socket PATH]\n\
                     default: NDJSON on stdin/stdout"
                );
                return;
            }
            other => {
                eprintln!("tuwaiq-agent-broker: unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    eprintln!("tuwaiq-agent-broker: starting (protocol v{PROTOCOL_VERSION})");
    if let Some(path) = socket_path {
        #[cfg(unix)]
        {
            if let Err(e) = serve_unix_socket(&path) {
                eprintln!("tuwaiq-agent-broker: socket server failed: {e}");
                std::process::exit(1);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            eprintln!("tuwaiq-agent-broker: --unix-socket is only supported on Unix");
            std::process::exit(2);
        }
    } else {
        serve_stdio();
    }
    eprintln!("tuwaiq-agent-broker: shutting down");
}

fn serve_stdio() {
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
        if write_response(&mut out, &handle_line(&line)).is_err() {
            eprintln!("tuwaiq-agent-broker: stdout closed, exiting read loop");
            break;
        }
    }
}

#[cfg(unix)]
fn serve_unix_socket(path: &str) -> io::Result<()> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::Path;

    let path_obj = Path::new(path);
    if let Some(parent) = path_obj.parent() {
        fs::create_dir_all(parent)?;
    }
    if path_obj.exists() {
        let _ = fs::remove_file(path_obj);
    }
    let listener = UnixListener::bind(path_obj)?;
    let _ = fs::set_permissions(path_obj, fs::Permissions::from_mode(0o666));
    eprintln!("tuwaiq-agent-broker: listening on {path}");

    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                if let Err(e) = handle_client(stream) {
                    eprintln!("tuwaiq-agent-broker: client session ended: {e}");
                }
            }
            Err(e) => {
                eprintln!("tuwaiq-agent-broker: accept error: {e}");
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn handle_client(stream: std::os::unix::net::UnixStream) -> io::Result<()> {
    use std::io::BufReader;

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        write_response(&mut writer, &handle_line(line.trim_end()))?;
    }
    Ok(())
}

fn write_response(out: &mut impl Write, response: &ToolResponse) -> io::Result<()> {
    let serialized = serde_json::to_string(response).unwrap_or_else(|_| {
        r#"{"status":"error","error":{"code":"internal_error","message":"failed to serialize response"}}"#
            .to_string()
    });
    writeln!(out, "{serialized}")?;
    out.flush()
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

    let decision = registry::policy_decision(&request.tool);
    match registry::dispatch(&request.tool, &request.arguments) {
        Ok(result) => {
            audit::record_with_policy(
                &request.request_id,
                &request.tool,
                &request.arguments,
                "ok",
                &decision,
                Some(false),
            );
            ToolResponse::ok(&request.request_id, result)
        }
        Err((code, message)) => {
            audit::record_with_policy(
                &request.request_id,
                &request.tool,
                &request.arguments,
                &format!("error:{code:?}"),
                &decision,
                Some(decision.requires_confirmation),
            );
            ToolResponse::error(&request.request_id, code, message)
        }
    }
}
