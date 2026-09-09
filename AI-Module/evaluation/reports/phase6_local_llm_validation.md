# Phase 6 Local LLM Validation and Benchmarking Report

- Timestamp: 2026-08-27T00:00:51.791350Z
- Local LLM layer status for Tuwaiq AI V1: **NOT READY**
- Why: not all required local Qwen models were benchmarked with real files in this environment

## 1. Test report

### UNIT TESTS

- Unit/integration coverage is exercised through the Python `agent/tests` suite and the Rust broker test suite.
- This generated report focuses on the runtime-facing Phase 6 acceptance and benchmark outcomes below.

### REAL LOCAL MODEL TESTS

### lite — qwen3.5-4b-instruct-quantized
- Availability: NOT AVAILABLE
- PASS/FAIL: NOT RUN
- Tested: False
- Hardware profile: device=cpu, threads=4, gpu_layers=0, min_ram_gb=8
- Runtime: llama.cpp
- Response quality: not_run
- Context handling: not_run
- Structured tool requests: not_run
- Security result: not_run
- Stability: not_run
- Issues:
  - Model file unavailable for profile 'lite' at /home/runner/work/TuwaiqOS/TuwaiqOS/AI-Module/models/local/qwen3.5-4b-quantized.gguf; benchmark not run.

### default — qwen3.5-9b-instruct-quantized
- Availability: NOT AVAILABLE
- PASS/FAIL: NOT RUN
- Tested: False
- Hardware profile: device=cpu, threads=6, gpu_layers=0, min_ram_gb=16
- Runtime: llama.cpp
- Response quality: not_run
- Context handling: not_run
- Structured tool requests: not_run
- Security result: not_run
- Stability: not_run
- Issues:
  - Model file unavailable for profile 'default' at /home/runner/work/TuwaiqOS/TuwaiqOS/AI-Module/models/local/qwen3.5-9b-quantized.gguf; benchmark not run.

### pro — qwen3.5-27b-instruct
- Availability: NOT AVAILABLE
- PASS/FAIL: NOT RUN
- Tested: False
- Hardware profile: device=cpu, threads=8, gpu_layers=0, min_ram_gb=48
- Runtime: llama.cpp
- Response quality: not_run
- Context handling: not_run
- Structured tool requests: not_run
- Security result: not_run
- Stability: not_run
- Issues:
  - Model file unavailable for profile 'pro' at /home/runner/work/TuwaiqOS/TuwaiqOS/AI-Module/models/local/qwen3.5-27b.gguf; benchmark not run.

## 2. Model benchmark report

### lite
- Startup time (ms): None
- Model loading time (ms): None
- Inference latency (ms): None
- RAM usage (MB): None
- VRAM usage (MB): None
- CPU usage (%): None
- GPU usage (%): None
- Tool-calling success: None

### default
- Startup time (ms): None
- Model loading time (ms): None
- Inference latency (ms): None
- RAM usage (MB): None
- VRAM usage (MB): None
- CPU usage (%): None
- GPU usage (%): None
- Tool-calling success: None

### pro
- Startup time (ms): None
- Model loading time (ms): None
- Inference latency (ms): None
- RAM usage (MB): None
- VRAM usage (MB): None
- CPU usage (%): None
- GPU usage (%): None
- Tool-calling success: None

## 3. Security test report

- no shell access: PASS — Tool schemas expose only typed tool arguments and no raw shell-command parameter.
- no unrestricted subprocess execution: PASS — Rust launches only compiled-in allowlisted applications and does not invoke a shell.
- no root: PASS — No tool requests privilege escalation or root acquisition.
- no direct OS access: PASS — The Python agent loop does not shell out or read host state directly.
- no cloud AI dependency: PASS — The agent/model stack is implemented around LocalModelProvider + llama.cpp only.
- no bypass around Rust broker: PASS — Tool execution in the Python agent is routed through BrokerClient, preserving the Rust boundary.
- sensitive operations require confirmation: PASS — Sensitive close/kill tools are exposed to the model, but the active agent loop must hold them in a pending confirmation state until a separate explicit approval turn arrives.
- TuwaiqOS remains usable if AI crashes: PASS — Phase 5 crash-isolation and restart hooks are present for the local runtime.

## 4. End-to-end CLI demo results

- lite: CLI demo not run because the configured model file was unavailable.
- default: CLI demo not run because the configured model file was unavailable.
- pro: CLI demo not run because the configured model file was unavailable.

## 5. Known limitations

- Real Phase 6 benchmarks were not available for: default, lite, pro.
- VRAM/GPU metrics remain unavailable on CPU-only or not-run validations and must be re-collected on target hardware.

## 6. Recommendation for default model

- Keep Qwen3.5-9B Quantized as the intended default profile label for now, but do not promote any model as the Tuwaiq AI V1 default until the new Phase 6 runner is executed against all three real local model files on target hardware.

## 7. List of issues discovered

- Model file unavailable for profile 'lite' at /home/runner/work/TuwaiqOS/TuwaiqOS/AI-Module/models/local/qwen3.5-4b-quantized.gguf; benchmark not run.
- Model file unavailable for profile 'default' at /home/runner/work/TuwaiqOS/TuwaiqOS/AI-Module/models/local/qwen3.5-9b-quantized.gguf; benchmark not run.
- Model file unavailable for profile 'pro' at /home/runner/work/TuwaiqOS/TuwaiqOS/AI-Module/models/local/qwen3.5-27b.gguf; benchmark not run.

## Final security confirmations

- no shell access: confirmed
- no unrestricted subprocess execution: confirmed
- no root: confirmed
- no direct OS access: confirmed
- no cloud AI dependency: confirmed
- no bypass around Rust broker: confirmed
- sensitive operations require confirmation: confirmed
- TuwaiqOS remains usable if AI crashes: confirmed

## Ready verdict: NOT READY

not all required local Qwen models were benchmarked with real files in this environment
