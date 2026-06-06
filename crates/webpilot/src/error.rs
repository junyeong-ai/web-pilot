//! Structured error type for WebPilot.
//!
//! Each variant carries the data needed to render AI-friendly guidance via
//! `Display`. There is no message-string parsing anywhere — guidance is
//! computed from typed fields, so it remains stable across translations,
//! reformatting, or refactors.
//!
//! Wire format:
//!   `{ "code": "ElementNotFound", "message": "...", ...data }`
//! where `code` is the discriminator and `...data` are variant-specific
//! fields. `WireError` is the on-wire shim; `WebPilotError::from_wire`
//! reconstructs the typed variant tolerantly.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Structured error type. Use `Display` for AI-friendly guidance.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WebPilotError {
    #[error(
        "Element [{requested}] not found (page has [1]-[{available}]). Re-capture: webpilot capture --include dom"
    )]
    ElementNotFound { requested: u32, available: u32 },

    #[error("Selector not found: {selector}. Verify CSS selector syntax.")]
    SelectorNotFound { selector: String },

    #[error("{kind} timed out after {elapsed_ms}ms")]
    Timeout { kind: String, elapsed_ms: u64 },

    #[error("Navigation failed: {url}: {reason}")]
    NavigationFailed { url: String, reason: String },

    #[error("No web page open. Navigate: webpilot action navigate URL")]
    NoPage,

    #[error("Frame not found: {selector}. List frames: webpilot frame")]
    FrameNotFound { selector: String },

    #[error("Invalid argument: {detail}")]
    InvalidArgument { detail: String },

    #[error("Bridge not loaded. Try: webpilot capture --include dom")]
    BridgeUnavailable,

    #[error("Chrome connection lost: {detail}. Run: webpilot status")]
    ConnectionLost { detail: String },

    #[error("Blocked by policy: {operation}. Check: webpilot policy list")]
    PolicyDenied { operation: String },

    #[error("CSP blocks script injection. Use: webpilot dom get-text SELECTOR")]
    CspViolation,

    #[error("Tab not found: {tab_id}. List: webpilot tab")]
    TabNotFound { tab_id: String },

    #[error("Context not found: {name}. List: webpilot context list")]
    ContextNotFound { name: String },

    #[error("Session error: {detail}")]
    Session { detail: String },

    #[error("{detail}")]
    Other { detail: String },
}

impl WebPilotError {
    /// Stable CLI exit code per error kind.
    pub fn exit_code(&self) -> i32 {
        use WebPilotError as E;
        match self {
            E::ElementNotFound { .. }
            | E::SelectorNotFound { .. }
            | E::TabNotFound { .. }
            | E::ContextNotFound { .. }
            | E::FrameNotFound { .. } => 4,
            E::Timeout { .. } => 5,
            E::PolicyDenied { .. } | E::CspViolation => 6,
            E::ConnectionLost { .. } | E::BridgeUnavailable => 3,
            E::InvalidArgument { .. } => 7,
            E::NavigationFailed { .. } | E::NoPage => 8,
            E::Session { .. } | E::Other { .. } => 1,
        }
    }

    /// PascalCase code identifier (matches wire `code` field).
    pub fn code(&self) -> &'static str {
        use WebPilotError as E;
        match self {
            E::ElementNotFound { .. } => "ElementNotFound",
            E::SelectorNotFound { .. } => "SelectorNotFound",
            E::Timeout { .. } => "Timeout",
            E::NavigationFailed { .. } => "NavigationFailed",
            E::NoPage => "NoPage",
            E::FrameNotFound { .. } => "FrameNotFound",
            E::InvalidArgument { .. } => "InvalidArgument",
            E::BridgeUnavailable => "BridgeUnavailable",
            E::ConnectionLost { .. } => "ConnectionLost",
            E::PolicyDenied { .. } => "PolicyDenied",
            E::CspViolation => "CspViolation",
            E::TabNotFound { .. } => "TabNotFound",
            E::ContextNotFound { .. } => "ContextNotFound",
            E::Session { .. } => "Session",
            E::Other { .. } => "Other",
        }
    }

    /// Reconstruct a typed variant from the on-wire shape.
    /// Unknown codes map to `Other { detail: message }` — never fail to parse.
    pub fn from_wire(w: WireError) -> Self {
        let f = &w.data;
        let str_field = |k: &str| f.get(k).and_then(|v| v.as_str()).map(str::to_owned);
        let u32_field = |k: &str| f.get(k).and_then(|v| v.as_u64()).map(|v| v as u32);
        let u64_field = |k: &str| f.get(k).and_then(|v| v.as_u64());

        match w.code.as_str() {
            "ElementNotFound" => Self::ElementNotFound {
                requested: u32_field("requested").unwrap_or(0),
                available: u32_field("available").unwrap_or(0),
            },
            "SelectorNotFound" => Self::SelectorNotFound {
                selector: str_field("selector").unwrap_or(w.message),
            },
            "Timeout" => Self::Timeout {
                kind: str_field("kind").unwrap_or_else(|| "operation".into()),
                elapsed_ms: u64_field("elapsed_ms").unwrap_or(0),
            },
            "NavigationFailed" => Self::NavigationFailed {
                url: str_field("url").unwrap_or_default(),
                reason: str_field("reason").unwrap_or(w.message),
            },
            "NoPage" => Self::NoPage,
            "FrameNotFound" => Self::FrameNotFound {
                selector: str_field("selector").unwrap_or(w.message),
            },
            "InvalidArgument" => Self::InvalidArgument { detail: w.message },
            "BridgeUnavailable" => Self::BridgeUnavailable,
            "ConnectionLost" => Self::ConnectionLost { detail: w.message },
            "PolicyDenied" => Self::PolicyDenied {
                operation: str_field("operation").unwrap_or(w.message),
            },
            "CspViolation" => Self::CspViolation,
            "TabNotFound" => Self::TabNotFound {
                tab_id: str_field("tab_id").unwrap_or(w.message),
            },
            "ContextNotFound" => Self::ContextNotFound {
                name: str_field("name").unwrap_or(w.message),
            },
            "Session" | "SessionError" => Self::Session { detail: w.message },
            _ => Self::Other { detail: w.message },
        }
    }

    /// Project to wire shape for protocol responses.
    pub fn to_wire(&self) -> WireError {
        use WebPilotError as E;
        let mut data = serde_json::Map::new();
        let mut put = |k: &str, v: serde_json::Value| {
            data.insert(k.into(), v);
        };
        match self {
            E::ElementNotFound {
                requested,
                available,
            } => {
                put("requested", (*requested).into());
                put("available", (*available).into());
            }
            E::SelectorNotFound { selector } => put("selector", selector.clone().into()),
            E::Timeout { kind, elapsed_ms } => {
                put("kind", kind.clone().into());
                put("elapsed_ms", (*elapsed_ms).into());
            }
            E::NavigationFailed { url, reason } => {
                put("url", url.clone().into());
                put("reason", reason.clone().into());
            }
            E::FrameNotFound { selector } => put("selector", selector.clone().into()),
            E::PolicyDenied { operation } => put("operation", operation.clone().into()),
            E::TabNotFound { tab_id } => put("tab_id", tab_id.clone().into()),
            E::ContextNotFound { name } => put("name", name.clone().into()),
            _ => {}
        }
        WireError {
            code: self.code().to_owned(),
            message: self.to_string(),
            data,
        }
    }
}

impl From<WireError> for WebPilotError {
    fn from(w: WireError) -> Self {
        Self::from_wire(w)
    }
}

/// On-wire error shape. `code` discriminates; `data` carries typed fields
/// (flattened into the JSON object alongside `code` and `message`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireError {
    pub code: String,
    #[serde(default)]
    pub message: String,
    #[serde(flatten)]
    pub data: serde_json::Map<String, serde_json::Value>,
}

impl Serialize for WebPilotError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_wire().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WebPilotError {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_wire(WireError::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_not_found_message_carries_data() {
        let e = WebPilotError::ElementNotFound {
            requested: 5,
            available: 3,
        };
        let s = e.to_string();
        assert!(s.contains("[5]"));
        assert!(s.contains("[1]-[3]"));
        assert_eq!(e.exit_code(), 4);
    }

    #[test]
    fn from_wire_round_trips_known_code() {
        let original = WebPilotError::ElementNotFound {
            requested: 7,
            available: 4,
        };
        let wire = original.to_wire();
        let json = serde_json::to_value(&wire).unwrap();
        assert_eq!(json["code"], "ElementNotFound");
        assert_eq!(json["requested"], 7);
        assert_eq!(json["available"], 4);

        let recovered: WebPilotError = serde_json::from_value(json).unwrap();
        assert_eq!(recovered, original);
    }

    #[test]
    fn policy_denied_carries_operation_field() {
        let original = WebPilotError::PolicyDenied {
            operation: "eval".into(),
        };
        let json = serde_json::to_value(original.to_wire()).unwrap();
        assert_eq!(json["code"], "PolicyDenied");
        assert_eq!(json["operation"], "eval");

        let recovered: WebPilotError = serde_json::from_value(json).unwrap();
        assert_eq!(recovered, original);
        assert_eq!(recovered.exit_code(), 6);
    }

    #[test]
    fn from_wire_handles_unknown_code_via_other() {
        let json = serde_json::json!({"code": "MysteryCode", "message": "boom"});
        let e: WebPilotError = serde_json::from_value(json).unwrap();
        assert!(matches!(e, WebPilotError::Other { .. }));
        assert_eq!(e.to_string(), "boom");
    }

    #[test]
    fn exit_codes_partition_kinds() {
        let e = WebPilotError::Timeout {
            kind: "wait".into(),
            elapsed_ms: 1000,
        };
        assert_eq!(e.exit_code(), 5);

        let e = WebPilotError::InvalidArgument {
            detail: "bad".into(),
        };
        assert_eq!(e.exit_code(), 7);
    }
}
