# Dataset

## Current Dataset Status

No real TuwaiqOS telemetry dataset is currently available in this prototype.
All records are synthetic and clearly labeled with:
- source = synthetic
- is_synthetic_anomaly = true/false

## Telemetry Schema

Defined in:
- data_collection/schemas/telemetry.schema.json

Core fields:
- timestamp
- cpu_utilization_pct
- ram_utilization_pct
- available_ram_mb
- process_count
- disk_read_kbps
- disk_write_kbps
- network_in_kbps
- network_out_kbps
- uptime_seconds
- error_event_count
- service_state

## Synthetic Scenario Profiles

Normal baseline:
- Stable CPU/RAM usage
- Moderate IO/network
- Low error count
- healthy state

Anomaly scenarios:
- high_cpu
- high_memory
- high_disk_io
- high_network
- mixed_resource_anomaly

## Feature Definitions for Model

Used features:
- cpu_utilization_pct
- ram_utilization_pct
- available_ram_mb
- process_count
- disk_read_kbps
- disk_write_kbps
- network_in_kbps
- network_out_kbps
- uptime_seconds
- error_event_count
- service_state_code

Not used directly:
- timestamp (avoids time-identifier leakage)
- source
- scenario_label
- is_synthetic_anomaly (used for synthetic evaluation only)

## Dataset Versioning

Initial synthetic dataset version:
- synthetic-v1

Future versions should include:
- source provenance
- schema version
- collection policy version
- sampling configuration

## Future Real Data Collection Requirements

Future integration requirements:
- Read-only telemetry provider from TuwaiqOS/system APIs
- Stable sampling interface
- Policy constraints for data minimization
- Optional redaction and privacy controls
