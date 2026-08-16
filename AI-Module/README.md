# tuwaiq-telemetry-provider

Stage 1 (per `ai_development/docs/INTEGRATION.md`'s staging) System
Telemetry Provider. Emits one JSON object on stdout, built entirely from
real host `/proc` data, that conforms to
`ai_development/system_interface/schemas/telemetry_input.schema.json`.

This is the piece the integration guide calls "future integration
requirement" — it now exists, is read-only, and its output has been
verified (see `test_schema_conformance.py`) against the exact schema
already checked into the repo.

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
| `cpu_utilization_pct` | `/proc/stat`, 500ms sample | |
| `ram_utilization_pct`, `available_ram_mb` | `/proc/meminfo` | |
| `process_count` | count of numeric entries in `/proc` | |
| `disk_read_kbps`, `disk_write_kbps` | `/proc/diskstats`, 500ms sample | KB/s (kilobytes), excludes partitions/loop/ram devices to avoid double-counting |
| `network_in_kbps`, `network_out_kbps` | `/proc/net/dev`, 500ms sample | KB/s (kilobytes), excludes loopback |
| `uptime_seconds` | `/proc/uptime` | |
| `error_event_count` | `dmesg --level=err,warn --since=-2min` | **Best-effort.** Returns `0` if `dmesg` is unavailable or unreadable without privilege — `0` here means "no error source was sampled," not necessarily "no errors occurred." See `INTEGRATION.md`'s own convention for unavailable metrics. |
| `service_state` | threshold heuristic (`service_state.rs`) | v1 only — CPU/RAM/error-count thresholds, not a learned classifier. Documented as future work, same as `INTEGRATION.md` already anticipates. |

## Tests

```
cargo test                              # unit tests: service_state thresholds
python -m pytest test_schema_conformance.py -v   # schema validation against the real repo schema
```

Both suites run against the real compiled binary and real `/proc` data —
nothing here is mocked.
