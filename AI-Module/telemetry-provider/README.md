# tuwaiq-telemetry-provider

Stage 1 (per `ai_development/docs/INTEGRATION.md`'s staging) System
Telemetry Provider. Emits one JSON object on stdout, built from real host
system data via the `sysinfo` crate (works on both Windows and Linux), that
conforms to `ai_development/system_interface/schemas/telemetry_input.schema.json`.

This is the piece the integration guide calls "future integration
requirement" — it now exists, is read-only, and its output has been
verified (see `test_schema_conformance.py`) against the exact schema
already checked into the repo, on Windows (the platform this project's own
setup instructions target).

## Build

```
cd telemetry-provider
cargo build
```

## Run standalone

```
./target/debug/tuwaiq-telemetry-provider
```

Prints one schema-conforming JSON object and exits. Takes ~1.5 seconds
(three ~500ms sampling windows for CPU/disk/network rate metrics).

## Feed it into the existing inference pipeline

This is the actual round-trip the task asks for: real telemetry → the
already-implemented Isolation Forest model → a real anomaly result.

```
./target/debug/tuwaiq-telemetry-provider > /tmp/live_snapshot.json
cd ../ai_development
python inference/predict.py --input-json /tmp/live_snapshot.json
```

The output should be a JSON object conforming to
`system_interface/schemas/inference_output.schema.json` (`anomaly_detected`,
`anomaly_score`, `severity`, `model_version`, `indicators`, ...), computed
from a genuine live snapshot of this machine — not the synthetic training
data.

## Fields and known limitations

| Field | Source | Notes |
|---|---|---|
| `cpu_utilization_pct` | `sysinfo` global CPU usage, 600ms two-sample window | |
| `ram_utilization_pct`, `available_ram_mb` | `sysinfo` memory | |
| `process_count` | `sysinfo` process list length | |
| `disk_read_kbps`, `disk_write_kbps` | Sum of `Process::disk_usage()` (bytes since last refresh) across all processes | `sysinfo` does not expose per-device throughput cross-platform; this is the standard `sysinfo` pattern for aggregate system I/O (Linux via `/proc/<pid>/io`, Windows via `GetProcessIoCounters`) |
| `network_in_kbps`, `network_out_kbps` | `sysinfo` per-interface received/transmitted bytes since last refresh, summed, excluding loopback-like interfaces | |
| `uptime_seconds` | `sysinfo::System::uptime()` | |
| `error_event_count` | Unix: `dmesg --level=err,warn --since=-2min` line count. **Windows: not yet implemented — always 0.** | `0` means "no error source was sampled," not necessarily "no errors occurred." A real Windows implementation needs a Windows Event Log (System log, Error/Warning) query via the `windows` crate — documented here as a known gap, consistent with `INTEGRATION.md`'s own convention for unavailable metrics, rather than shipped as if it were real. |
| `service_state` | threshold heuristic (`service_state.rs`) | v1 only — CPU/RAM/error-count thresholds, not a learned classifier. Documented as future work, same as `INTEGRATION.md` already anticipates. |

## Tests

```
cargo test                              # unit tests: service_state thresholds
python -m pytest test_schema_conformance.py -v   # schema validation against the real repo schema
```

Both suites run against the real compiled binary and real `/proc` data —
nothing here is mocked.
