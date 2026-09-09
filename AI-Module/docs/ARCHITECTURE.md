# Tuwaiq AI Architecture

## Scope

This directory contains the current local-first Tuwaiq AI implementation for
TuwaiqOS. The authoritative control flow is:

```text
User → Tuwaiq AI UI/CLI → Python Agent → LocalModelProvider → Local Qwen Runtime
     → Structured Tool Request → Rust Broker → Permission/Policy → OS
```

The agent never executes shell commands or touches the OS directly.

## Components

### Python Agent

`agent/agent.py`

- runs the bounded reasoning loop
- validates tool names before they reach Rust
- holds pending confirmation state for sensitive actions
- routes every approved tool call through `BrokerClient`

### Conversation state

`agent/conversation_context.py`

- keeps a bounded transcript
- stores the last resolved entity/process list for follow-up turns
- stores the exact pending sensitive action awaiting confirmation

### Model abstraction

`agent/model_provider.py`

- `RuleBasedProvider` remains the deterministic fallback for tests and safe degradation
- `LocalModelProvider` keeps the existing abstraction and delegates to the local runtime
- model profile selection remains `lite` / `default` / `pro`

### Local Qwen runtime

`agent/local_model_runtime.py`

Default runtime path:

```text
Agent process
  ↓ IPC
isolated model runtime child process
  ↓
llama.cpp / llama-cpp-python
  ↓
local GGUF Qwen model
```

Lifecycle:

1. validate profile + model path
2. preflight RAM/VRAM checks
3. start isolated child runtime
4. wait for startup handshake
5. health check
6. send inference request with timeout
7. receive validated response
8. shutdown or terminate on failure

Handled failures:

- missing model file
- invalid model path
- startup failure
- invalid runtime response
- broken IPC
- inference timeout
- repeated crashes
- child process unexpected exit
- OOM where detectable

The runtime records process state plus load/inference telemetry and clears dead
backends so the agent does not stay blocked on a broken model process.

### Rust broker

`broker/src/*`

- final execution boundary for OS access
- fixed tool registry
- argument validation
- allowlist enforcement for app launch/close
- protected-process denial for `kill_process`
- audit logging

Rust remains the final authority for tool execution.

## Confirmation workflow

Sensitive tools currently include:

- `close_application`
- `kill_process`

Flow:

1. Model or fallback provider proposes the tool request.
2. Agent stores `{tool, arguments, description}` as a pending action.
3. Agent asks the user for explicit approval.
4. Only `yes/allow/approve/confirm` on the next turn executes the broker call.
5. `no/deny/reject/cancel` clears the request without execution.
6. Any other reply leaves the request pending.

This keeps approval tied to a specific action request and prevents the model
from self-authorizing.

## Model profiles

`agent/model_profiles.py`

- `lite` → Qwen3.5-4B quantized
- `default` → Qwen3.5-9B quantized (**default target**)
- `pro` → Qwen3.5-27B

The model files remain local-only. No cloud provider is required.

## Validation and benchmark flow

`agent/phase6_validation.py`

The Phase 6 runner:

- probes `lite`, `default`, and `pro`
- reports model availability as `AVAILABLE`, `NOT AVAILABLE`, or `FAILED`
- performs real local inference only when the model file exists
- records latency/resource telemetry where available
- verifies diagnosis, follow-up context, and confirmation-gated close flow
- writes markdown and JSON reports under `evaluation/`

## Test entry points

From `AI-Module/`:

```bash
python -m pytest agent/tests -q
cd broker && cargo test
python agent/phase6_validation.py
python agent/cli.py
```
