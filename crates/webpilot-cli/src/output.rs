use std::io::IsTerminal;

use webpilot::types::{DomSnapshot, ErrorCode, ProtocolError, WebPilotError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Human,
    Json,
}

pub fn detect_output_mode(force_json: bool) -> OutputMode {
    if force_json || !std::io::stdout().is_terminal() {
        OutputMode::Json
    } else {
        OutputMode::Human
    }
}

// ── Unified command result types ─────────────────────────────────────────────

/// Unified return type from all command handlers.
/// Handlers return this instead of doing their own output formatting.
/// Use with `render()` in dispatch layer — handlers never see OutputMode.
pub enum CommandOutput {
    /// Simple success with optional message: "OK", "Switched to tab X", etc.
    Ok(String),

    /// Success with structured JSON data + human-readable summary.
    Data {
        json: serde_json::Value,
        human: String,
    },

    /// DOM snapshot (special case: to_text for Human, JSON for Json).
    Dom {
        snapshot: DomSnapshot,
        extra: serde_json::Map<String, serde_json::Value>,
    },

    /// Content to print to stdout (e.g., cookie value, eval result, DOM text).
    Content {
        stdout: String,
        json: serde_json::Value,
    },

    /// List of items with human-readable lines and JSON representation.
    List {
        items: serde_json::Value,
        human_lines: Vec<String>,
        summary: String,
    },

    /// Silent success (e.g., quit).
    Silent,
}

/// Render a successful command result to stdout/stderr.
pub fn render(result: CommandOutput, mode: OutputMode) {
    match (result, mode) {
        (CommandOutput::Ok(msg), OutputMode::Human) => eprintln!("{msg}"),
        (CommandOutput::Ok(_), OutputMode::Json) => println!("{{\"success\":true}}"),

        (CommandOutput::Data { human, .. }, OutputMode::Human) => eprintln!("{human}"),
        (CommandOutput::Data { json, .. }, OutputMode::Json) => {
            println!("{}", serde_json::to_string_pretty(&json).unwrap_or_default())
        }

        (CommandOutput::Dom { snapshot, extra }, OutputMode::Human) => {
            if !snapshot.elements.is_empty() {
                print!("{}", snapshot.to_text());
            }
            if let Some(path) = extra.get("screenshot_path").and_then(|v| v.as_str()) {
                eprintln!("Screenshot: {path}");
            }
            if let Some(path) = extra.get("pdf_path").and_then(|v| v.as_str()) {
                eprintln!("PDF: {path}");
            }
        }
        (CommandOutput::Dom { snapshot, extra }, OutputMode::Json) => {
            let mut obj = serde_json::to_value(&snapshot)
                .unwrap_or(serde_json::Value::Object(Default::default()));
            if let Some(map) = obj.as_object_mut() {
                for (k, v) in &extra {
                    map.insert(k.clone(), v.clone());
                }
            }
            println!("{obj}");
        }

        (CommandOutput::Content { stdout, .. }, OutputMode::Human) => println!("{stdout}"),
        (CommandOutput::Content { json, .. }, OutputMode::Json) => println!("{json}"),

        (CommandOutput::List { human_lines, summary, .. }, OutputMode::Human) => {
            for line in &human_lines {
                eprintln!("{line}");
            }
            if !summary.is_empty() {
                eprintln!("{summary}");
            }
        }
        (CommandOutput::List { items, .. }, OutputMode::Json) => {
            println!("{}", serde_json::to_string_pretty(&items).unwrap_or_default())
        }

        (CommandOutput::Silent, _) => {}
    }
}

/// Render a WebPilotError to stderr (Human) or JSON (Json).
pub fn render_error(err: &WebPilotError, mode: OutputMode) {
    let protocol_err = ProtocolError {
        message: err.message.clone(),
        code: err.code.clone(),
    };
    match mode {
        OutputMode::Human => eprintln!("{}", format_error(&protocol_err)),
        OutputMode::Json => println!(
            "{}",
            serde_json::json!({"success": false, "error": {"message": err.message, "code": err.code.to_string()}})
        ),
    }
}

// ── Error formatting (AI-friendly guidance) ──────────────────────────────────

/// Transform raw error messages into AI-friendly guidance with actionable next steps.
pub fn format_error(err: &ProtocolError) -> String {
    match err.code {
        ErrorCode::ElementNotFound => {
            let numbers: Vec<&str> = err
                .message
                .split(|c: char| !c.is_ascii_digit())
                .filter(|s| !s.is_empty())
                .collect();
            if numbers.len() >= 3 {
                let idx = numbers[0];
                let max = numbers[2];
                return format!(
                    "Element [{idx}] not found (page has [1]-[{max}]). Re-capture: webpilot capture --dom"
                );
            }
            format!("{}. Re-capture: webpilot capture --dom", err.message)
        }
        ErrorCode::SelectorNotFound => {
            format!("Selector not found: {}. Verify CSS selector syntax.", err.message)
        }
        ErrorCode::Timeout => "Timed out. Try: webpilot wait --timeout 15".into(),
        ErrorCode::PolicyDenied => "Blocked by policy. Check: webpilot policy list".into(),
        ErrorCode::CSPViolation => {
            "CSP blocks script injection. Use: webpilot dom get-text SELECTOR".into()
        }
        ErrorCode::NoPage => {
            "No web page open. Navigate: webpilot action navigate URL".into()
        }
        ErrorCode::NavigationFailed => {
            format!("Navigation failed: {}. Check URL and retry.", err.message)
        }
        ErrorCode::FrameNotFound => {
            "Frame not found. List frames: webpilot frames list".into()
        }
        ErrorCode::InvalidArgument => format!("Invalid argument: {}", err.message),
        ErrorCode::BridgeUnavailable => {
            "Bridge not loaded. Try: webpilot capture --dom (re-injects bridge)".into()
        }
        ErrorCode::ConnectionLost => "Chrome connection lost. Run: webpilot status".into(),
        ErrorCode::TabNotFound => "Tab not found. List: webpilot tabs list".into(),
        ErrorCode::ContextNotFound => "Context not found. List: webpilot context list".into(),
        ErrorCode::SessionError => format!("Session error: {}", err.message),
        ErrorCode::Unknown => format_error_str(&err.message),
    }
}

/// Format a plain error string with actionable guidance.
pub fn format_error_str(msg: &str) -> String {
    if msg.contains("No web page tab") {
        "No web page open. Navigate: webpilot capture --dom --url URL".into()
    } else if msg.contains("not running") || msg.contains("Not connected") {
        "Not connected. Run: webpilot install, then reload Chrome extension".into()
    } else if msg.contains("Content Security Policy") || msg.contains("CSP") {
        "CSP blocks eval. Use: webpilot dom get-text SELECTOR".into()
    } else {
        msg.to_string()
    }
}
