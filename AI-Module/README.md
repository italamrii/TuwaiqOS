# Tuwaiq AI Module

Local-first Tuwaiq AI prototype for `ibraman5/TuwaiqOS`.

## Current architecture

```text
User
  ↓
Tuwaiq AI UI/CLI
  ↓
Python Agent
  ↓
LocalModelProvider
  ↓
Local Qwen Runtime
  ↓
Structured Tool Request
  ↓
Rust Broker
  ↓
Permission / Policy
  ↓
OS
```

Security invariants:

- no cloud AI dependency
- no shell-command tool
- all OS actions go through the Rust broker
- sensitive actions require explicit confirmation
- the model cannot approve its own action
- model/runtime failure must not crash the broker or the OS

## Model profiles

`agent/model_profiles.py` defines three local Qwen profiles:

- `lite` → Qwen3.5-4B quantized
- `default` → Qwen3.5-9B quantized (**current default target**)
- `pro` → Qwen3.5-27B

Model paths can be configured with:

- `TUWAIQ_AI_MODEL_ROOT`
- `TUWAIQ_AI_MODEL_PATH_LITE`
- `TUWAIQ_AI_MODEL_PATH_DEFAULT`
- `TUWAIQ_AI_MODEL_PATH_PRO`

## Confirmation workflow

Sensitive tools such as `close_application` and `kill_process` are model-visible,
but the Python agent does not execute them immediately.

Flow:

1. Model proposes a structured sensitive tool request.
2. Agent stores the exact pending action in memory.
3. Agent asks for explicit `yes/allow/approve` or `no/deny`.
4. Only an explicit approval on a later turn causes the Rust broker call.
5. A denial cancels the stored action.

## Process isolation and crash handling

The default local Qwen runtime uses a separate Python child process for the
model runtime behind a narrow IPC interface:

```text
Python Agent → LocalModelProvider → isolated model runtime process → llama.cpp / Qwen
```

The parent runtime performs:

- startup handshake
- health check
- timed inference request
- unexpected-exit detection
- bounded repeated-crash protection
- shutdown / forced termination on timeout

If the model process crashes, hangs, or returns invalid data, the agent falls
back safely without direct OS access.

## Main commands

From `AI-Module/`:

```bash
python -m pytest agent/tests -q
cd broker && cargo test
python agent/phase6_validation.py
python agent/cli.py
```

## Real local benchmark

`python agent/phase6_validation.py` writes:

- `evaluation/reports/phase6_local_llm_validation.md`
- `evaluation/benchmarks/phase6_local_llm_validation.json`

Per profile it reports:

- availability: `AVAILABLE` / `NOT AVAILABLE` / `FAILED`
- PASS/FAIL verdict for the real validation run
- runtime, hardware profile, latency, RAM/VRAM, CPU/GPU where available
- tool-calling, structured request validity, context reuse, security, stability

Missing model files are reported as unavailable; results are never fabricated.

## CLI acceptance scenario

When the required model file is present, the intended manual CLI flow is:

1. `Why is my computer slow?`
2. `What's using the most?`
3. `Close it.`
4. explicit confirmation: `yes` or denial: `no`

The diagnosis and final action must stay grounded in broker-returned telemetry.
