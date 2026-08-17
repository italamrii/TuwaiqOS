# Local runtime measurements (Grounded Agent V1)

Record only what is demonstrated on this host.

## Host capacity

- System RAM: ~16 GiB
- NVIDIA VRAM: 8151 MiB (RTX 5050 Laptop GPU)
- Runtime: Ollama 0.32.13 (OpenAI-compatible at `http://127.0.0.1:11434/v1`)

## Installed model (live verification 2026-08-16/17)

| Field | Value |
|---|---|
| Identifier | `qwen3.5:9b` |
| Digest / ID | `6488c96fa5fa` |
| Source | Official Ollama library (`ollama pull qwen3.5:9b`) corresponding to Qwen3.5-9B |
| License | Apache-2.0 |
| Parameters | 9.7B |
| Quantization | Q4_K_M |
| Package size | 6.6 GB (model blob 6594462816 bytes) |
| Capabilities | completion, vision, tools, thinking |
| Backend | NVIDIA GPU offload (measured ~6500–6570 MiB VRAM in use while loaded) |

## Live Arabic acceptance measurements

| Metric | Measured value |
|---|---|
| Warmup / first local response | 111.829 s |
| Primary request `ليش جهازي بطيء؟` | 100.987 s |
| Agent iterations (primary) | 4 (budget) |
| Tool calls (primary) | `get_cpu_info` → `get_memory_info` → `get_disk_info` → `list_processes` |
| Follow-up `وش أكثر برنامج مستهلك؟` | 18.644 s; reused fresh evidence (0 new tools) |
| Close intent `سكره` | confirmation required for `tuwaiq-agent-br` (pid 1); no termination executed |
| Provider outage | bounded `provider_unavailable` in 2.04 s; no crash; no false answer |
| Provider class | `LocalModelProvider` (not `RuleBasedProvider`) |
| Broker | Linux `tuwaiq-agent-broker:live` via Docker for real `/proc` telemetry |

## Notes

- Broker telemetry in this Windows-host verification is the Docker Linux broker environment (real `/proc` evidence), not a rewritten agent architecture.
- Model weights remain outside Git under the Ollama store.
- This stack is still **not production-ready**.
