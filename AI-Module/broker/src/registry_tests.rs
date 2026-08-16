//! Unit tests for registry dispatch and policy classification.

#[cfg(test)]
mod tests {
    use crate::protocol::{ErrorCode, RiskClass};
    use crate::registry::{classify, dispatch, policy_decision, tool_catalog_json};
    use serde_json::json;

    #[test]
    fn unknown_tool_is_rejected() {
        let result = dispatch("delete_everything", &json!({}));
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::UnknownTool);
    }

    #[test]
    fn forbidden_shell_tool_is_permission_denied() {
        let result = dispatch("run_shell", &json!({"cmd": "rm -rf /"}));
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::PermissionDenied);
        assert_eq!(classify("run_shell"), RiskClass::Forbidden);
    }

    #[test]
    fn terminate_process_is_sensitive_and_not_executed() {
        let decision = policy_decision("terminate_process");
        assert!(!decision.allowed);
        assert!(decision.requires_confirmation);
        assert_eq!(decision.risk, RiskClass::SensitiveAction);
        let result = dispatch("terminate_process", &json!({"pid": 1}));
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::PermissionDenied);
    }

    #[test]
    fn read_only_tool_rejects_nonempty_arguments() {
        let result = dispatch("get_cpu_info", &json!({"unexpected": "field"}));
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidArguments);
    }

    #[test]
    fn read_only_tool_accepts_empty_arguments() {
        let result = dispatch("get_system_info", &json!({}));
        assert!(result.is_ok());
    }

    #[test]
    fn read_only_tool_accepts_null_arguments() {
        let result = dispatch("get_system_info", &serde_json::Value::Null);
        assert!(result.is_ok());
    }

    #[test]
    fn launch_application_without_app_id_is_invalid_arguments() {
        let result = dispatch("launch_application", &json!({}));
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidArguments);
    }

    #[test]
    fn launch_application_with_unknown_app_id_is_denied() {
        let result = dispatch("launch_application", &json!({"app_id": "definitely_not_real"}));
        assert!(result.is_err());
        let (code, message) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotAllowlisted);
        assert!(message.contains("definitely_not_real"));
    }

    #[test]
    fn launch_application_never_treats_app_id_as_a_shell_command() {
        let result = dispatch(
            "launch_application",
            &json!({"app_id": "firefox; rm -rf /"}),
        );
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotAllowlisted);
    }

    #[test]
    fn app_id_wrong_type_is_invalid_arguments_not_a_panic() {
        let result = dispatch("launch_application", &json!({"app_id": 12345}));
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidArguments);
    }

    #[test]
    fn catalog_lists_exactly_the_six_phase1_tools() {
        let catalog = tool_catalog_json();
        let tools = catalog.get("tools").and_then(|t| t.as_array()).unwrap();
        assert_eq!(tools.len(), 6);
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t.get("name").and_then(|n| n.as_str()).unwrap())
            .collect();
        assert!(names.contains(&"get_memory_info"));
        assert!(names.contains(&"launch_application"));
        assert!(!names.contains(&"terminate_process"));
        assert!(!names.contains(&"run_shell"));
    }

    #[test]
    fn read_tools_are_classified_as_read() {
        assert_eq!(classify("get_cpu_info"), RiskClass::Read);
        assert_eq!(classify("list_processes"), RiskClass::Read);
        assert_eq!(classify("launch_application"), RiskClass::LowRiskAction);
    }
}
