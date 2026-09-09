//! tuwaiq-telemetry-provider -- Stage 1 read-only System Telemetry
//! Provider.
//!
//! Emits exactly one JSON object on stdout, matching
//! `ai_development/system_interface/schemas/telemetry_input.schema.json`,
//! built entirely from real host system data via `sysinfo` (works on both
//! Windows and Linux -- see `readers.rs`'s module docs for why an earlier
//! `/proc`-only implementation was replaced). Intended to be piped into a
//! file and consumed by the existing `inference/predict.py --input-json
//! <file>` pipeline -- see `README.md` in this directory for the full
//! round-trip demo.
//!
//! This binary is read-only: it does not accept any input, take any
//! action, or write anything except its own stdout snapshot.

mod readers;
mod service_state;
#[cfg(test)]
mod service_state_tests;
mod snapshot;

use snapshot::TelemetrySnapshot;

fn main() {
    let timestamp = chrono::Utc::now().to_rfc3339();

    let samples = readers::take_samples();
    let error_event_count = readers::read_recent_error_event_count();

    let state = service_state::derive_service_state(
        samples.cpu_utilization_pct,
        samples.ram_utilization_pct,
        error_event_count,
    );

    let snapshot = TelemetrySnapshot {
        timestamp,
        cpu_utilization_pct: samples.cpu_utilization_pct,
        ram_utilization_pct: samples.ram_utilization_pct,
        available_ram_mb: samples.available_ram_mb,
        process_count: samples.process_count,
        disk_read_kbps: samples.disk_read_kbps,
        disk_write_kbps: samples.disk_write_kbps,
        network_in_kbps: samples.network_in_kbps,
        network_out_kbps: samples.network_out_kbps,
        uptime_seconds: samples.uptime_seconds,
        error_event_count,
        service_state: state,
        source: "future_real",
        scenario_label: "live_snapshot",
        is_synthetic_anomaly: false,
    };

    match serde_json::to_string_pretty(&snapshot) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("tuwaiq-telemetry-provider: failed to serialize snapshot: {e}");
            std::process::exit(1);
        }
    }
}