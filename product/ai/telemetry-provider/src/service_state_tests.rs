#[cfg(test)]
mod tests {
    use crate::service_state::derive_service_state;
    use crate::snapshot::ServiceState;

    #[test]
    fn healthy_thresholds() {
        assert_eq!(derive_service_state(10.0, 20.0, 0), ServiceState::Healthy);
    }

    #[test]
    fn degraded_on_high_cpu() {
        assert_eq!(derive_service_state(75.0, 20.0, 0), ServiceState::Degraded);
    }

    #[test]
    fn degraded_on_high_ram() {
        assert_eq!(derive_service_state(10.0, 85.0, 0), ServiceState::Degraded);
    }

    #[test]
    fn critical_on_very_high_cpu() {
        assert_eq!(derive_service_state(95.0, 20.0, 0), ServiceState::Critical);
    }

    #[test]
    fn critical_on_very_high_ram() {
        assert_eq!(derive_service_state(10.0, 95.0, 0), ServiceState::Critical);
    }

    #[test]
    fn critical_on_many_errors() {
        assert_eq!(derive_service_state(10.0, 20.0, 20), ServiceState::Critical);
    }

    #[test]
    fn degraded_on_some_errors() {
        assert_eq!(derive_service_state(10.0, 20.0, 5), ServiceState::Degraded);
    }
}
