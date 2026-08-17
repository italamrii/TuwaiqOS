//! Integration test: runs the real compiled `tuwaiq-agent-broker` binary as
//! a subprocess (exactly how `broker_client.py` uses it) and feeds it a
//! mixed sequence of malformed, unknown, denied, and valid requests on one
//! stdin stream. Asserts:
//!   1. Every line produces exactly one JSON response line.
//!   2. The process does not exit or panic partway through.
//!   3. A malformed line earlier in the stream does not corrupt handling
//!      of valid lines that come after it.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn broker_binary_path() -> std::path::PathBuf {
    // `cargo test` places the test binary next to the debug build; the
    // main binary is at target/debug/tuwaiq-agent-broker regardless of
    // which specific path this test binary itself lives at.
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("tuwaiq-agent-broker");
    path
}

#[test]
fn survives_malformed_unknown_denied_and_then_answers_valid_requests() {
    let bin = broker_binary_path();
    assert!(bin.exists(), "broker binary not found at {bin:?}; run `cargo build` first");

    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn broker");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    let inputs = [
        "this is not json",
        r#"{"protocol_version":"1.0","request_id":"a","timestamp":"t","tool":"delete_everything","arguments":{}}"#,
        r#"{"protocol_version":"1.0","request_id":"b","timestamp":"t","tool":"launch_application","arguments":{"app_id":"nope"}}"#,
        r#"{"protocol_version":"9.9","request_id":"c","timestamp":"t","tool":"get_system_info","arguments":{}}"#,
        r#"{"protocol_version":"1.0","request_id":"d","timestamp":"t","tool":"get_system_info","arguments":{}}"#,
    ];

    let mut responses = Vec::new();
    for input in inputs {
        writeln!(stdin, "{input}").expect("write to broker stdin");
        stdin.flush().expect("flush");

        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read from broker stdout");
        assert!(n > 0, "broker produced no output for input: {input}");
        responses.push(line.trim().to_string());
    }

    // Process must still be alive after all five inputs, including the
    // malformed one and the two rejected ones -- proves nothing panicked
    // or exited early.
    let still_running = child.try_wait().expect("try_wait").is_none();
    assert!(still_running, "broker exited before all requests were answered");

    let parsed: Vec<serde_json::Value> = responses
        .iter()
        .map(|r| serde_json::from_str(r).unwrap_or_else(|e| panic!("response wasn't valid JSON: {r} ({e})")))
        .collect();

    assert_eq!(parsed[0]["error"]["code"], "malformed_request");
    assert_eq!(parsed[1]["error"]["code"], "unknown_tool");
    assert_eq!(parsed[2]["error"]["code"], "not_allowlisted");
    assert_eq!(parsed[3]["error"]["code"], "unsupported_protocol_version");
    assert_eq!(parsed[4]["status"], "ok");
    assert_eq!(parsed[4]["result"]["os_name"], "TuwaiqOS");

    drop(stdin);
    let _ = child.wait();
}
