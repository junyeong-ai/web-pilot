//! Command output rendering.
//!
//! Handlers return `CommandOutput`; the dispatch layer calls `render` to
//! emit JSON or human-readable text. Errors render via `Display` on the
//! structured `WebPilotError` — there is no message-string parsing.

use std::io::IsTerminal;
use webpilot::WebPilotError;
use webpilot::types::DomSnapshot;

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

/// Unified return type from all command handlers.
pub enum CommandOutput {
    Ok(String),
    Data {
        json: serde_json::Value,
        human: String,
    },
    Dom {
        snapshot: DomSnapshot,
        extra: serde_json::Map<String, serde_json::Value>,
    },
    Content {
        stdout: String,
        json: serde_json::Value,
    },
    List {
        items: serde_json::Value,
        human_lines: Vec<String>,
        summary: String,
    },
    Silent,
}

pub fn render(result: CommandOutput, mode: OutputMode) {
    match (result, mode) {
        (CommandOutput::Ok(msg), OutputMode::Human) => eprintln!("{msg}"),
        (CommandOutput::Ok(_), OutputMode::Json) => println!(r#"{{"success":true}}"#),

        (CommandOutput::Data { human, .. }, OutputMode::Human) => eprintln!("{human}"),
        (CommandOutput::Data { json, .. }, OutputMode::Json) => emit_json(&json),

        (CommandOutput::Dom { snapshot, extra }, OutputMode::Human) => {
            if !snapshot.elements.is_empty() {
                print!("{}", snapshot.to_text());
            }
            for (key, label) in [
                ("screenshot_path", "Screenshot"),
                ("pdf_path", "PDF"),
                ("accessibility_path", "Accessibility tree"),
            ] {
                if let Some(p) = extra.get(key).and_then(|v| v.as_str()) {
                    eprintln!("{label}: {p}");
                }
            }
        }
        (CommandOutput::Dom { snapshot, extra }, OutputMode::Json) => {
            let mut value = serde_json::to_value(&snapshot)
                .unwrap_or_else(|_| serde_json::Value::Object(Default::default()));
            if let Some(map) = value.as_object_mut() {
                for (k, v) in extra {
                    map.insert(k, v);
                }
            }
            emit_json(&value);
        }

        (CommandOutput::Content { stdout, .. }, OutputMode::Human) => println!("{stdout}"),
        (CommandOutput::Content { json, .. }, OutputMode::Json) => emit_json(&json),

        (
            CommandOutput::List {
                human_lines,
                summary,
                ..
            },
            OutputMode::Human,
        ) => {
            for line in &human_lines {
                eprintln!("{line}");
            }
            if !summary.is_empty() {
                eprintln!("{summary}");
            }
        }
        (CommandOutput::List { items, .. }, OutputMode::Json) => emit_json(&items),

        (CommandOutput::Silent, _) => {}
    }
}

fn emit_json(value: &serde_json::Value) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            // Serialization failure of an in-memory Value is essentially
            // impossible, but we surface it explicitly rather than swallow.
            eprintln!("Internal: failed to serialize output: {e}");
            println!(r#"{{"success":false,"error":"output serialization failed"}}"#);
        }
    }
}

/// Render an error to stderr (Human) or stdout (Json).
///
/// Guidance text comes from `Display` on the structured error variant — the
/// data is the source of truth, the message is derived. There is no
/// fallback that inspects the message text for substrings.
pub fn render_error(err: &WebPilotError, mode: OutputMode) {
    match mode {
        OutputMode::Human => eprintln!("{err}"),
        OutputMode::Json => {
            let wire = err.to_wire();
            match serde_json::to_string(&wire) {
                Ok(s) => println!(r#"{{"success":false,"error":{s}}}"#),
                Err(_) => println!(
                    r#"{{"success":false,"error":{{"code":"Other","message":"output serialization failed"}}}}"#
                ),
            }
        }
    }
}
