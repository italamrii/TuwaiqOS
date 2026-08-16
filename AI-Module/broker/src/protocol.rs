//! Wire types for the Tuwaiq Agent Broker Protocol v1.
//!
//! These types are the Rust mirror of `protocol/schema.json`. Deserialization
//! is deliberately strict (`deny_unknown_fields`) so a malformed or
//! unexpected request is rejected as `malformed_request` rather than
//! silently ignoring fields the sender got wrong.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "1.0";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRequest {
    pub protocol_version: String,
    pub request_id: String,
    pub timestamp: String,
    pub tool: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ToolResponse {
    pub protocol_version: String,
    pub request_id: String,
    pub timestamp: String,
    pub status: ResponseStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResponseStatus {
    Ok,
    Error,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    MalformedRequest,
    UnsupportedProtocolVersion,
    UnknownTool,
    InvalidArguments,
    PermissionDenied,
    NotAllowlisted,
    NotFound,
    InternalError,
}

/// Risk class for broker policy decisions. Mirrored by the Python agent for
/// confirmation UX; enforcement remains in Rust.
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RiskClass {
    Read,
    LowRiskAction,
    SensitiveAction,
    Forbidden,
}

impl ToolResponse {
    pub fn ok(request_id: &str, result: serde_json::Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            request_id: request_id.to_string(),
            timestamp: now_rfc3339(),
            status: ResponseStatus::Ok,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(request_id: &str, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            request_id: request_id.to_string(),
            timestamp: now_rfc3339(),
            status: ResponseStatus::Error,
            result: None,
            error: Some(ErrorBody {
                code,
                message: message.into(),
            }),
        }
    }

    /// A malformed_request response with no known request_id, for the case
    /// where the incoming line wasn't even valid JSON / valid ToolRequest
    /// shape, so we have nothing reliable to echo back.
    pub fn malformed(raw_error: &str) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_string(),
            request_id: "unknown".to_string(),
            timestamp: now_rfc3339(),
            status: ResponseStatus::Error,
            result: None,
            error: Some(ErrorBody {
                code: ErrorCode::MalformedRequest,
                // Deliberately generic: never echo the raw parser error
                // (which can include fragments of the offending input) back
                // over a channel that may be logged or displayed.
                message: format!("request could not be parsed: {raw_error}"),
            }),
        }
    }
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}
