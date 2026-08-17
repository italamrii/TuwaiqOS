# Product AI ISO proof (Developer live)

Date: 2026-08-17 (UTC+3 host clock Aug 16 evening)

## Build

| Field | Value |
|---|---|
| Branch | `product/tuwaiq-ai-integration` |
| Base | `product/developer-iso` @ `e076981` |
| Accepted AI | `ai/local-agent-v1` @ `98aa734` |
| ISO | `product/out/TuwaiqOS-Developer-x86_64.iso` |
| ISO SHA-256 | `fb8b66713672458ceea3ac7397e4457a132b6a40b1b9ced8be0527c31656eb08` |
| ISO size | 775706624 bytes (~740 MiB) |

## Boot proof (QEMU TCG, no GPU)

Script: `product/out/verify-ai-iso.ps1`  
Log: `product/out/ai-verify/boot-serial.log`  
Results: `product/out/ai-verify/results.json`

| Check | Result |
|---|---|
| Serial login | PASS |
| `tuwaiq-agent-broker.service` enabled+active | PASS |
| `tuwaiq-ai.service` enabled+active | PASS |
| `/run/tuwaiq/ai-broker.sock` | PASS |
| `/run/tuwaiq/ai.sock` | PASS |
| `tuwaiq-ai status` → `MODEL_NOT_INSTALLED` | PASS |
| Ollama absent from ISO | PASS |
| Plasma (`plasmashell`) + `kwin_x11` alive | PASS |
| SDDM active | PASS |

Guest status excerpt:

```json
{
  "state": "MODEL_NOT_INSTALLED",
  "model_id": "qwen3.5:9b",
  "provider": "local",
  "ollama_present": false,
  "ollama_reachable": false,
  "model_installed": false,
  "broker_reachable": true,
  "detail": "ollama runtime not installed or not running"
}
```

## Honesty boundary

- This Product proof does **not** claim live Qwen answers inside QEMU.
- Qwen live acceptance remains host-side evidence in `product/ai/docs/LOCAL_RUNTIME.md`.
- ISO intentionally excludes Ollama and model weights; users must run
  `tuwaiq-ai provision-model` on a machine with Ollama installed.
