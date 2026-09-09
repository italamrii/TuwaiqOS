//! `service_state` derivation.
//!
//! The schema requires this field but does not define how it's computed --
//! this is a simple, transparent threshold heuristic (v1), not a claim of
//! diagnostic accuracy. It exists so the field is always populated with
//! something meaningful rather than a constant placeholder; refining these
//! thresholds (or replacing them with a learned classifier) is exactly the
//! kind of "future integration requirement" system_interface/docs already
//! calls out for metrics that don't yet have a real source.

use crate::snapshot::ServiceState;

pub fn derive_service_state(
    cpu_utilization_pct: f64,
    ram_utilization_pct: f64,
    error_event_count: u64,
) -> ServiceState {
    if cpu_utilization_pct > 90.0 || ram_utilization_pct > 90.0 || error_event_count > 10 {
        ServiceState::Critical
    } else if cpu_utilization_pct > 70.0 || ram_utilization_pct > 80.0 || error_event_count > 3 {
        ServiceState::Degraded
    } else {
        ServiceState::Healthy
    }
}
