# Tuwaiq AI Threat Model

## Core assumption

The model layer is untrusted input. It may hallucinate, be prompt-injected, or
emit malformed tool requests. The design therefore treats model output like any
other untrusted request and keeps the Rust broker as the final OS boundary.

## Security goals

- no arbitrary shell execution
- no direct OS access from the model
- no root escalation path
- sensitive actions require explicit confirmation
- model/runtime failure must not take down the broker or the OS
- no cloud dependency for the local Qwen path

## Main threats and mitigations

### T1: Model attempts arbitrary shell execution

Mitigation:

- there is no shell tool
- Python validates tool names against `KNOWN_TOOLS`
- tool schemas expose only typed arguments
- Rust launches only fixed allowlisted applications, never a shell

Covered by:

- `agent/tests/test_tool_calling.py`
- `agent/tests/test_phase6_validation.py`

### T2: Model invents a tool or bypasses the registry

Mitigation:

- Python rejects unknown tool names before broker dispatch
- Rust `registry.rs` independently rejects unknown tools

Covered by:

- `agent/tests/test_tool_calling.py`
- `broker/src/registry_tests.rs`

### T3: Model requests a destructive action without explicit approval

Mitigation:

- `close_application` and `kill_process` are intercepted by the agent
- the agent stores the exact pending action in `pending_confirmation`
- only a later explicit `yes/allow/approve/confirm` executes the broker call
- denial cancels the pending action

Covered by:

- `agent/tests/test_agent_loop.py`

### T4: Confirmed destructive action still targets something unsafe

Mitigation:

- Rust independently rejects protected targets such as pid 1 and protected
  process names
- confirmation is a UX safety gate, not the only security control

Covered by:

- `broker/src/registry_tests.rs`

### T5: Model runtime crashes, hangs, or returns invalid data

Mitigation:

- local Qwen runs in a separate child process by default
- parent runtime performs startup handshake and health checks
- inference is bounded by timeout
- unexpected child exit, invalid response, and broken IPC become controlled
  runtime errors
- repeated crash protection prevents infinite restart loops

Covered by:

- `agent/tests/test_phase5_reliability.py`

### T6: Agent process fails

Mitigation:

- critical OS state is not stored only inside the agent
- a new agent/broker client can continue using the Rust broker path
- the OS does not depend on the model process remaining alive

Covered by:

- `agent/tests/test_phase5_reliability.py`

## Out of scope

- broker sandboxing beyond the current narrow tool registry
- network telemetry export
- GUI-specific attack surface
- cloud-provider threat models
