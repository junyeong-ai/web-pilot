//! MCP (Model Context Protocol) server — `webpilot mcp`.
//!
//! Speaks JSON-RPC 2.0 over stdio (newline-delimited UTF-8; stdout is
//! MCP-only, stderr is logs) so any MCP host can drive the same browser the
//! CLI drives. It is a thin adapter: each tool builds a typed `Command` /
//! `Action` and runs it through the existing command handler over the shared
//! `Transport`, so mode parity, policy enforcement, and the agent-facing DOM
//! rendering are all inherited — never reimplemented. The transport opens once
//! and is reused for the whole session, keeping the browser warm across calls.

use anyhow::Result;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use webpilot::WebPilotError;
use webpilot::action::Action;
use webpilot::capture::{CaptureField, CaptureOpts};
use webpilot::protocol::{Command, ResponseData};
use webpilot::wait::WaitCondition;

use crate::commands::{action, capture, eval};
use crate::transport::{IpcTransport, LocalTransport, Transport};

/// Latest MCP protocol revision this server implements.
const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(clap::Args)]
pub struct McpArgs {}

/// Run the stdio MCP server in the same mode the CLI would use (`--browser` /
/// `--context` are honored by the caller).
pub async fn serve(browser: bool, context: Option<String>) -> Result<()> {
    if browser {
        run(&mut IpcTransport::new()).await
    } else {
        run(&mut LocalTransport::open(context.as_deref()).await?).await
    }
}

async fn run<T: Transport>(transport: &mut T) -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let Some(reply) = handle_line(transport, &line).await else {
            continue; // notification or unaddressable input — no response
        };
        let mut buf = serde_json::to_string(&reply).expect("JSON-RPC reply serializes");
        buf.push('\n');
        stdout.write_all(buf.as_bytes()).await?;
        stdout.flush().await?;
    }
    Ok(())
}

/// Route one JSON-RPC message. Returns `None` for notifications (no `id`) and
/// for input we can't address a reply to.
async fn handle_line<T: Transport>(transport: &mut T, line: &str) -> Option<Value> {
    let msg: Value = serde_json::from_str(line).ok()?;
    let id = msg.get("id").filter(|v| !v.is_null()).cloned()?;
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "initialize" => Some(ok_reply(id, initialize_result(&params))),
        "ping" => Some(ok_reply(id, json!({}))),
        "tools/list" => Some(ok_reply(id, json!({ "tools": tool_specs() }))),
        "tools/call" => Some(tool_call_reply(transport, id, &params).await),
        other => Some(error_reply(
            id,
            -32601,
            &format!("method not found: {other}"),
        )),
    }
}

fn ok_reply(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize_result(params: &Value) -> Value {
    // Echo the client's requested protocol version when present (maximal
    // compatibility); otherwise advertise ours.
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "webpilot", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// A tool failure is reported as a successful JSON-RPC response carrying
/// `isError: true`, so the model sees the message and can react, per the MCP
/// tool-call contract. Only malformed requests use JSON-RPC-level errors.
async fn tool_call_reply<T: Transport>(transport: &mut T, id: Value, params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let result = match call_tool(transport, name, &args).await {
        Ok(text) => json!({
            "content": [{ "type": "text", "text": text }],
            "isError": false,
        }),
        Err(e) => json!({
            "content": [{ "type": "text", "text": format!("{e}") }],
            "isError": true,
        }),
    };
    ok_reply(id, result)
}

async fn call_tool<T: Transport>(
    transport: &mut T,
    name: &str,
    args: &Value,
) -> std::result::Result<String, WebPilotError> {
    let output = match name {
        "browser_navigate" => {
            capture::run(
                transport,
                capture::CaptureArgs {
                    include: vec![CaptureField::Dom],
                    url: Some(str_arg(args, "url")?),
                    opts: CaptureOpts::default(),
                },
            )
            .await
        }
        "browser_snapshot" => {
            capture::run(
                transport,
                capture::CaptureArgs {
                    include: vec![CaptureField::Dom],
                    url: None,
                    opts: CaptureOpts::default(),
                },
            )
            .await
        }
        "browser_screenshot" => {
            capture::run(
                transport,
                capture::CaptureArgs {
                    include: vec![CaptureField::Screenshot],
                    url: None,
                    opts: CaptureOpts::default(),
                },
            )
            .await
        }
        "browser_click" | "browser_type" | "browser_press_key" | "browser_scroll"
        | "browser_select" => {
            let action = build_action(name, args)?;
            action::run(
                transport,
                action::ActionArgs {
                    action,
                    capture: true,
                },
            )
            .await
        }
        "browser_eval" => {
            eval::run(
                transport,
                eval::EvalArgs {
                    code: str_arg(args, "code")?,
                },
            )
            .await
        }
        "browser_wait" => return wait_tool(transport, args).await,
        other => {
            return Err(WebPilotError::InvalidArgument {
                detail: format!("unknown tool: {other}"),
            });
        }
    };
    output
        .map(|o| o.to_agent_text())
        .map_err(crate::into_webpilot_error)
}

/// Build a typed `Action` from a tool's arguments by injecting the wire `kind`
/// discriminator and reusing `Action`'s own deserialization — so every default
/// (`clear`, scroll `amount`, key `modifiers`) and validation comes from the
/// single source, not a parallel mapping here.
fn build_action(tool: &str, args: &Value) -> std::result::Result<Action, WebPilotError> {
    let kind = match tool {
        "browser_click" => "click",
        "browser_type" => "type",
        "browser_press_key" => "key_press",
        "browser_scroll" => "scroll",
        "browser_select" => "select",
        _ => unreachable!("build_action only called for action tools"),
    };
    let mut obj = args.as_object().cloned().unwrap_or_default();
    obj.insert("kind".into(), json!(kind));
    serde_json::from_value(Value::Object(obj)).map_err(|e| WebPilotError::InvalidArgument {
        detail: format!("invalid arguments for {tool}: {e}"),
    })
}

async fn wait_tool<T: Transport>(
    transport: &mut T,
    args: &Value,
) -> std::result::Result<String, WebPilotError> {
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(10_000);
    // Reuse WaitCondition's tagged deserialization; keep only its fields so an
    // extra `timeout_ms` can't trip a future stricter schema.
    let mut cond = serde_json::Map::new();
    if let Some(until) = args.get("until") {
        cond.insert("until".into(), until.clone());
    }
    if let Some(value) = args.get("value") {
        cond.insert("value".into(), value.clone());
    }
    let condition: WaitCondition =
        serde_json::from_value(Value::Object(cond)).map_err(|e| WebPilotError::InvalidArgument {
            detail: format!("invalid wait condition: {e}"),
        })?;

    let result = transport
        .send(Command::Wait {
            condition,
            timeout_ms,
        })
        .await
        .map_err(crate::into_webpilot_error)?;
    match result {
        ResponseData::Wait { success: true, .. } => Ok("OK".into()),
        ResponseData::Wait { error, .. } => Err(error.unwrap_or(WebPilotError::Other {
            detail: "wait failed".into(),
        })),
        ResponseData::Error { error } => Err(error),
        _ => Err(WebPilotError::Other {
            detail: "unexpected response to wait".into(),
        }),
    }
}

fn str_arg(args: &Value, key: &str) -> std::result::Result<String, WebPilotError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| WebPilotError::InvalidArgument {
            detail: format!("missing required string argument: {key}"),
        })
}

/// The exposed tool surface, with JSON Schemas. Curated to the core
/// observe→act→observe agent loop; each acting tool auto-captures the resulting
/// DOM so the model always gets a fresh snapshot back.
fn tool_specs() -> Value {
    let index = json!({
        "type": "integer",
        "minimum": 1,
        "description": "Element index from the most recent snapshot's [N] markers.",
    });
    json!([
        {
            "name": "browser_navigate",
            "description": "Navigate to a URL and return the page's interactive-element snapshot.",
            "inputSchema": {
                "type": "object",
                "properties": { "url": { "type": "string", "description": "Absolute URL to load." } },
                "required": ["url"],
            },
        },
        {
            "name": "browser_snapshot",
            "description": "Re-capture the current page's interactive-element snapshot (the [N]-indexed list used to address elements).",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "browser_screenshot",
            "description": "Capture a screenshot of the current page; returns the saved image path.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "browser_click",
            "description": "Click the element at the given snapshot index, then return the updated snapshot.",
            "inputSchema": {
                "type": "object",
                "properties": { "index": index },
                "required": ["index"],
            },
        },
        {
            "name": "browser_type",
            "description": "Type text into the element at the given index, then return the updated snapshot.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index": index,
                    "text": { "type": "string", "description": "Text to type." },
                    "clear": { "type": "boolean", "description": "Replace the existing value instead of appending.", "default": false },
                },
                "required": ["index", "text"],
            },
        },
        {
            "name": "browser_press_key",
            "description": "Press a key (e.g. Enter, Tab, Escape, ArrowDown) on the focused element.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "Key name, e.g. \"Enter\" or a single character." },
                    "modifiers": {
                        "type": "object",
                        "description": "Optional modifier keys held during the press.",
                        "properties": {
                            "ctrl": { "type": "boolean" },
                            "shift": { "type": "boolean" },
                            "alt": { "type": "boolean" },
                            "meta": { "type": "boolean" },
                        },
                    },
                },
                "required": ["key"],
            },
        },
        {
            "name": "browser_scroll",
            "description": "Scroll the page up or down by a pixel amount.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "direction": { "type": "string", "enum": ["up", "down"] },
                    "amount": { "type": "integer", "minimum": 1, "description": "Pixels to scroll.", "default": 600 },
                },
                "required": ["direction"],
            },
        },
        {
            "name": "browser_select",
            "description": "Select an option by value in the <select> element at the given index.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index": index,
                    "value": { "type": "string", "description": "The option value to select." },
                },
                "required": ["index", "value"],
            },
        },
        {
            "name": "browser_eval",
            "description": "Evaluate JavaScript in the page (MAIN world) and return the JSON-serialized result.",
            "inputSchema": {
                "type": "object",
                "properties": { "code": { "type": "string", "description": "JavaScript expression or statements." } },
                "required": ["code"],
            },
        },
        {
            "name": "browser_wait",
            "description": "Wait for a condition before continuing.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "until": { "type": "string", "enum": ["selector", "text", "navigation", "idle"] },
                    "value": { "type": "string", "description": "Selector or text (required for until=selector|text)." },
                    "timeout_ms": { "type": "integer", "minimum": 1, "default": 10000 },
                },
                "required": ["until"],
            },
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_echoes_client_protocol_version() {
        let r = initialize_result(&json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(r["protocolVersion"], "2024-11-05");
        assert_eq!(r["serverInfo"]["name"], "webpilot");
        assert!(r["capabilities"]["tools"].is_object());
    }

    #[test]
    fn initialize_defaults_protocol_version_when_absent() {
        let r = initialize_result(&json!({}));
        assert_eq!(r["protocolVersion"], PROTOCOL_VERSION);
    }

    #[test]
    fn tool_specs_cover_the_agent_loop_with_object_schemas() {
        let specs = tool_specs();
        let names: Vec<&str> = specs
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for expected in [
            "browser_navigate",
            "browser_snapshot",
            "browser_click",
            "browser_type",
            "browser_eval",
            "browser_wait",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
        for tool in specs.as_array().unwrap() {
            assert_eq!(tool["inputSchema"]["type"], "object", "{} schema", tool["name"]);
        }
    }

    #[test]
    fn build_action_fills_wire_defaults() {
        // `type` without `clear` → clear defaults to false.
        let a = build_action("browser_type", &json!({ "index": 3, "text": "hi" })).unwrap();
        assert_eq!(
            a,
            Action::Type {
                index: 3,
                text: "hi".into(),
                clear: false,
            }
        );
        // `scroll` without `amount` → the shared default applies.
        let a = build_action("browser_scroll", &json!({ "direction": "down" })).unwrap();
        assert!(matches!(
            a,
            Action::Scroll {
                direction: webpilot::action::ScrollDir::Down,
                ..
            }
        ));
        // `key_press` without modifiers → defaults, no error.
        let a = build_action("browser_press_key", &json!({ "key": "Enter" })).unwrap();
        assert!(matches!(a, Action::KeyPress { .. }));
    }

    #[test]
    fn build_action_rejects_wrong_types() {
        let err = build_action("browser_click", &json!({ "index": "not-a-number" }));
        assert!(matches!(err, Err(WebPilotError::InvalidArgument { .. })));
    }
}
