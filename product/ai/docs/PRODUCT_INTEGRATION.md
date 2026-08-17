# Tuwaiq AI Product Integration

Userspace-only packaging of the accepted Grounded Local Agent V1 into the
Developer ISO. The Phase 8 kernel `ai_bridge` stub is **not** modified.

## Authority path

```text
Future GUI / tuwaiq-ai CLI
        → local Unix socket (/run/tuwaiq/ai.sock)
        → tuwaiq-ai.service (Agent + policy + confirmation)
        → LocalModelProvider (optional Ollama / qwen3.5:9b)
        → tuwaiq-agent-broker.service (/run/tuwaiq/ai-broker.sock)
        → native Product /proc telemetry (+ optional D1 connectivity JSON)
```

The GUI contract: a future panel is a socket client. It renders final
responses or `ACTION_CONFIRMATION_REQUIRED`. It never reaches Ollama or the
broker directly.

## Services

| Unit | Role |
|---|---|
| `tuwaiq-agent-broker.service` | Broker authority (Unix socket NDJSON) |
| `tuwaiq-ai.service` | Agent + local API (non-boot-critical) |

Both use bounded restart limits, `NoNewPrivileges`, private temp, resource
caps, and `WantedBy=multi-user.target` without `Before=` display-manager /
Plasma units.

## Status states

`MODEL_NOT_INSTALLED` | `MODEL_LOADING` | `MODEL_READY` | `READY` | `ERROR`

The shipped Developer ISO does **not** embed Ollama or Qwen weights. Boot
never pulls a model. Expect `MODEL_NOT_INSTALLED` / runtime-unavailable until
the user runs:

```bash
tuwaiq-ai provision-model
# requires a locally installed Ollama; then: ollama pull qwen3.5:9b
tuwaiq-ai status
tuwaiq-ai chat
```

## CLI

`tuwaiq-ai` talks **only** to the service socket (`status`, `chat`,
`provision-model`). It does not construct a parallel agent path.

## Source pins

| Item | Value |
|---|---|
| Product base | `product/developer-iso` @ `e076981` |
| Accepted AI | `ai/local-agent-v1` @ `98aa734` |
| Integration branch | `product/tuwaiq-ai-integration` |
| Model tag | `qwen3.5:9b` |

Live Qwen acceptance evidence remains host-side (`docs/LOCAL_RUNTIME.md`).
Product ISO proof is safe failure / status honesty under QEMU without GPU.
