# Grounded Local Tuwaiq Agent V1

Continuation of PR #28 host-side `AI-Module` agent/broker. This is **not**
production-ready and is **not** wired into the TuwaiqOS kernel `ai_bridge`
(Phase 8 stub remains untouched).

## Architecture

```text
User → BoundedAgent (max 4 iterations)
         → LocalModelProvider | RuleBasedProvider
         → Schema + Policy validation
         → Rust broker (authority)
         → Timestamped trusted evidence
         → Grounded response
```

| Layer | Trust |
|---|---|
| User text / model prose | Untrusted |
| Broker tool results | Trusted evidence while fresh (default TTL 120s) |
| Confirmation state | External to model text only |

## Configuration

| Env var | Default | Meaning |
|---|---|---|
| `TUWAIQ_AI_PROVIDER` | `local` | `local` or `rule` |
| `TUWAIQ_AI_OPENAI_BASE_URL` | `http://127.0.0.1:11434/v1` | OpenAI-compatible base URL |
| `TUWAIQ_AI_OPENAI_API_KEY` | `local` | Bearer token for local runtimes |
| `TUWAIQ_AI_MODEL` | `Qwen/Qwen3.5-4B` | Model id (9B is the product target; 4B for host verification) |
| `TUWAIQ_AI_TIMEOUT_SECONDS` | `60` | Provider HTTP timeout |
| `TUWAIQ_AI_MAX_ITERATIONS` | `4` | Tool/decision loop budget |
| `TUWAIQ_AI_EVIDENCE_TTL_SECONDS` | `120` | Evidence freshness |
| `TUWAIQ_AI_BROKER_PATH` | broker debug binary | Override broker path |

## Tools and policy

Callable broker tools (fixed): `get_system_info`, `get_cpu_info`,
`get_memory_info`, `get_disk_info`, `list_processes`, `launch_application`.

| Risk | Examples | Behavior |
|---|---|---|
| `READ` | telemetry tools | Allowed |
| `LOW_RISK_ACTION` | allowlisted launch | Allowed; broker allowlist enforced |
| `SENSITIVE_ACTION` | `terminate_process` intent | Confirmation UX, then **deferred** — not implemented in V1 broker |
| `FORBIDDEN` | shell / execute_command | Denied |

No arbitrary shell, dbus, filesystem, or process-kill capability is exposed.

## Session memory

- Bounded turns + evidence records (CLI session scope).
- Live state answers must come from fresh broker evidence.
- Stale evidence expires; it is not treated as current indefinitely.

## RAG boundary (V1)

**Not implemented.** V1 uses live broker evidence only. Planned later:
optional retrieval over local docs with explicit untrusted labeling, never
as a substitute for live system state.

## Build notes (Windows host)

The TuwaiqOS repo root enables Cargo `build-std` for the kernel. Build the
broker with a normal host toolchain from a path that does not inherit that
config (or clear it), e.g. copy `AI-Module/broker` to a temp directory and
run `cargo +stable test`. On Linux, `cargo test` inside `broker/` works once
the crate is excluded from the root workspace (see root `Cargo.toml`
`workspace.exclude`).

## Run

```bash
cd AI-Module/broker && cargo build && cargo test
cd ../agent
TUWAIQ_AI_PROVIDER=rule python -m pytest tests -v
TUWAIQ_AI_PROVIDER=rule python cli.py
```

For a real local model, start an OpenAI-compatible server that supports tool
calls, point `TUWAIQ_AI_OPENAI_BASE_URL` / `TUWAIQ_AI_MODEL` at it, and set
`TUWAIQ_AI_PROVIDER=local`.

## Local verification notes

See `docs/LOCAL_RUNTIME.md` for host measurements (model id, license,
latency, RAM/VRAM). Values are recorded only for what actually runs.

## Non-goals / honesty

- Not production-ready.
- Not integrated into OS GUI or kernel AI bridge.
- Process termination is intentionally unavailable.
- Model weights and broker `target/` artifacts must not be committed.
