//! Unit tests for `registry::dispatch` covering the required failure modes:
//! unknown tool, invalid arguments, and (via `tools::launch_application`)
//! denied/not-allowlisted action.

#[cfg(test)]
mod tests {
    use crate::protocol::ErrorCode;
    use crate::registry::dispatch;
    use serde_json::json;

    #[test]
    fn unknown_tool_is_rejected() {
        let result = dispatch("delete_everything", &json!({}));
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::UnknownTool);
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
        // A hostile/hallucinated request trying to smuggle a shell command
        // through app_id must fail as not_allowlisted, exactly like any
        // other unrecognized string -- there is no code path that would
        // ever pass this value to a shell.
        let result = dispatch(
            "launch_application",
            &json!({"app_id": "firefox; rm -rf /"}),
        );
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotAllowlisted);
    }

    #[test]
    fn get_network_status_accepts_empty_arguments() {
        let result = dispatch("get_network_status", &json!({}));
        assert!(result.is_ok());
    }
	    #[test]
    fn close_application_without_app_id_is_invalid_arguments() {
        let result = dispatch("close_application", &json!({}));
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidArguments);
    }

    #[test]
    fn close_application_unknown_app_is_not_allowlisted() {
        let result = dispatch("close_application", &json!({"app_id": "totally_fake"}));
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotAllowlisted);
    }

    #[test]
    fn close_application_valid_but_not_running_is_not_found() {
        let result = dispatch("close_application", &json!({"app_id": "vscode"}));
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[test]
    fn kill_process_refuses_pid_1_unconditionally() {
        // Regression test for a real bug found during development: name-
        // matching alone missed pid 1 in an environment where it wasn't
        // named "init" or "systemd". This must be denied by pid, not name.
        let result = dispatch("kill_process", &json!({"pid": 1}));
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::PermissionDenied);
    }

    #[test]
    fn kill_process_without_pid_is_invalid_arguments() {
        let result = dispatch("kill_process", &json!({}));
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidArguments);
    }

    #[test]
    fn kill_process_nonexistent_pid_is_not_found() {
        let result = dispatch("kill_process", &json!({"pid": 999_999}));
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::NotFound);
    }

    #[test]
    fn app_id_wrong_type_is_invalid_arguments_not_a_panic() {
        let result = dispatch("launch_application", &json!({"app_id": 12345}));
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ErrorCode::InvalidArguments);
    }
}
