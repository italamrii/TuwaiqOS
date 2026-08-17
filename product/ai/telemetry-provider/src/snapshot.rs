//! Wire type matching
//! `ai_development/system_interface/schemas/telemetry_input.schema.json`
//! field-for-field. Field names, types, and the `service_state` enum values
//! here are deliberately identical to the schema so this struct's
//! `serde_json` output validates against it without any translation layer.

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct TelemetrySnapshot {
    pub timestamp: String,
    pub cpu_utilization_pct: f64,
    pub ram_utilization_pct: f64,
    pub available_ram_mb: f64,
    pub process_count: u64,
    pub disk_read_kbps: f64,
    pub disk_write_kbps: f64,
    pub network_in_kbps: f64,
    pub network_out_kbps: f64,
    pub uptime_seconds: f64,
    pub error_event_count: u64,
    pub service_state: ServiceState,
    pub source: &'static str,
    pub scenario_label: &'static str,
    pub is_synthetic_anomaly: bool,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceState {
    Healthy,
    Degraded,
    Critical,
    Unknown,
}
