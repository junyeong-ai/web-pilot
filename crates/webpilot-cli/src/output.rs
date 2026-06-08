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
}

/// The side-channel artefacts a capture reports next to the DOM, with their
/// human labels. Single source for both the CLI renderer and `to_agent_text`.
const DOM_EXTRA_LABELS: [(&str, &str); 6] = [
    // Page identity first — a screenshot/PDF/AX-only capture has no DOM footer,
    // so these are how the agent learns what page the artifact actually shows
    // (after a redirect, or when an iframe is the active frame).
    ("page_url", "Page"),
    ("page_title", "Title"),
    ("screenshot_path", "Screenshot"),
    ("screenshot_error", "Screenshot failed"),
    ("pdf_path", "PDF"),
    ("accessibility_path", "Accessibility tree"),
];

/// One `Label: value` line per present capture artefact, in a stable order.
/// The single source for the CLI renderer, the MCP text block, and the capture
/// handler's no-DOM path.
pub(crate) fn dom_extra_lines(extra: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    DOM_EXTRA_LABELS
        .iter()
        .filter_map(|(key, label)| {
            extra
                .get(*key)
                .and_then(|v| v.as_str())
                .map(|v| format!("{label}: {v}"))
        })
        .collect()
}

impl CommandOutput {
    /// Flatten to one agent-facing text block — the body of an MCP tool result.
    /// Mirrors the Human render but returns the text instead of splitting it
    /// across stdout/stderr, so the MCP surface and the CLI share one set of
    /// renderers (`DomSnapshot::to_text`, the handler-built `human` strings).
    pub(crate) fn to_agent_text(&self) -> String {
        match self {
            CommandOutput::Ok(msg) => msg.clone(),
            CommandOutput::Data { human, .. } => human.clone(),
            CommandOutput::Content { stdout, .. } => stdout.clone(),
            CommandOutput::Dom { snapshot, extra } => {
                // The snapshot text is included even with zero interactive
                // elements: it carries the page header (title/URL/scroll),
                // which is exactly what an agent needs after landing on a
                // sparse page — an empty reply would hide where it is.
                let mut lines = vec![snapshot.to_text().trim_end().to_string()];
                lines.extend(dom_extra_lines(extra));
                lines.join("\n")
            }
            CommandOutput::List {
                human_lines,
                summary,
                ..
            } => {
                let mut lines = human_lines.clone();
                if !summary.is_empty() {
                    lines.push(summary.clone());
                }
                lines.join("\n")
            }
        }
    }
}

pub fn render(result: CommandOutput, mode: OutputMode) {
    match (result, mode) {
        (CommandOutput::Ok(msg), OutputMode::Human) => eprintln!("{msg}"),
        (CommandOutput::Ok(_), OutputMode::Json) => println!(r#"{{"success":true}}"#),

        (CommandOutput::Data { human, .. }, OutputMode::Human) => eprintln!("{human}"),
        (CommandOutput::Data { json, .. }, OutputMode::Json) => emit_json(&json),

        (CommandOutput::Dom { snapshot, extra }, OutputMode::Human) => {
            // Always render the snapshot text, even with zero interactive
            // elements: the `--- Page / Scroll / iframe ---` footer carries the
            // URL, title, and scroll context an agent needs to orient, and the
            // MCP path emits it unconditionally — human mode must not diverge.
            print!("{}", snapshot.to_text());
            for line in dom_extra_lines(&extra) {
                eprintln!("{line}");
            }
        }
        (CommandOutput::Dom { snapshot, extra }, OutputMode::Json) => {
            let mut value =
                serde_json::to_value(&snapshot).expect("DomSnapshot serializes (static shape)");
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
