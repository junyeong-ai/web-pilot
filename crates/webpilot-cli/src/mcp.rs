//! MCP (Model Context Protocol) server — `webpilot mcp`.
//!
//! Speaks JSON-RPC 2.0 over stdio (newline-delimited UTF-8; stdout is
//! MCP-only, stderr is logs) so any MCP host can drive the same browser the
//! CLI drives. It is a thin adapter: each tool builds a typed `Command` /
//! `Action` and runs it through the existing command handler over the shared
//! `Transport`, so mode parity, policy enforcement, and the agent-facing DOM
//! rendering are all inherited — never reimplemented.
//!
//! The transport opens lazily on the first tool call and is then reused for
//! the whole session: `initialize` and `tools/list` must answer even when
//! Chrome is slow or unavailable, and a failed launch belongs to the tool call
//! that needed the browser, not to the server's lifetime.

use std::future::Future;

use anyhow::Result;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use webpilot::WebPilotError;
use webpilot::action::Action;
use webpilot::capture::{CaptureField, CaptureOpts};

use crate::commands::{action, capture, eval, wait};
use crate::output::CommandOutput;
use crate::transport::{IpcTransport, LocalTransport, Transport};

/// The MCP protocol revision this server implements. Per the MCP lifecycle,
/// `initialize` always answers with a version the server supports — the client
/// decides whether to continue.
const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(clap::Args)]
pub struct McpArgs {}

/// Run the stdio MCP server in the same mode the CLI would use (`--browser` /
/// `--context` are honored by the caller).
pub async fn serve(browser: bool, context: Option<String>) -> Result<()> {
    if browser {
        run(|| async { anyhow::Ok(IpcTransport::new()) }).await
    } else {
        run(move || {
            let context = context.clone();
            async move { LocalTransport::open(context.as_deref()).await }
        })
        .await
    }
}

async fn run<T, F, Fut>(connect: F) -> Result<()>
where
    T: Transport,
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    // Bound each request line: a JSON-RPC message from the host is small, so a
    // line that grows past this cap — or never terminates — is malformed input,
    // not a request. `read_until` on a `take`-limited reader keeps one read from
    // growing memory without bound; an over-cap line is answered with a parse
    // error and skipped rather than buffered whole.
    const MAX_LINE_BYTES: u64 = 8 * 1024 * 1024;
    let mut transport: Option<T> = None;
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = (&mut reader)
            .take(MAX_LINE_BYTES + 1)
            .read_until(b'\n', &mut buf)
            .await?;
        if n == 0 {
            break; // EOF
        }
        let over_cap = buf.len() as u64 > MAX_LINE_BYTES && buf.last() != Some(&b'\n');
        let reply = if over_cap {
            Some(error_reply(
                Value::Null,
                -32700,
                "request exceeds size limit",
            ))
        } else {
            // Strict UTF-8: a JSON-RPC frame is UTF-8 by spec, so decode rather
            // than lossily coerce invalid bytes into U+FFFD and parse garbage.
            match std::str::from_utf8(&buf) {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => handle_line(&connect, &mut transport, line).await,
                Err(_) => Some(error_reply(
                    Value::Null,
                    -32700,
                    "parse error: invalid UTF-8",
                )),
            }
        };
        let Some(reply) = reply else {
            continue; // a notification — no response
        };
        let mut out = serde_json::to_string(&reply).expect("JSON-RPC reply serializes");
        out.push('\n');
        stdout.write_all(out.as_bytes()).await?;
        stdout.flush().await?;
    }
    Ok(())
}

/// Route one JSON-RPC message. Returns `None` only for a notification — a
/// request with NO `id` member at all. An explicit `id: null` is a request and
/// is answered (with a null id), distinct from an absent id.
async fn handle_line<T, F, Fut>(connect: &F, transport: &mut Option<T>, line: &str) -> Option<Value>
where
    T: Transport,
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        // JSON-RPC 2.0: a parse error is answered with a null id, never dropped.
        return Some(error_reply(Value::Null, -32700, "parse error"));
    };
    // Absent id = notification (no response). Present id (including null) = a
    // request that must be answered, echoing the id back.
    let id = msg.get("id").cloned()?;
    // JSON-RPC 2.0 requires `"jsonrpc": "2.0"`; a missing or different version
    // is an invalid request (-32600).
    if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(error_reply(
            id,
            -32600,
            "invalid request: jsonrpc must be \"2.0\"",
        ));
    }
    // A request carrying an id but no string `method` is malformed: JSON-RPC
    // 2.0 answers that with -32600 (invalid request), distinct from -32601
    // (a well-formed call to a method that does not exist).
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        return Some(error_reply(id, -32600, "invalid request: missing method"));
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "initialize" => Some(ok_reply(id, initialize_result())),
        "ping" => Some(ok_reply(id, json!({}))),
        "tools/list" => Some(ok_reply(id, json!({ "tools": tool_specs() }))),
        "tools/call" => Some(tool_call_reply(connect, transport, id, &params).await),
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

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "webpilot", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// A tool failure is reported as a successful JSON-RPC response carrying
/// `isError: true`, so the model sees the message and can react, per the MCP
/// tool-call contract. Only malformed requests use JSON-RPC-level errors.
async fn tool_call_reply<T, F, Fut>(
    connect: &F,
    transport: &mut Option<T>,
    id: Value,
    params: &Value,
) -> Value
where
    T: Transport,
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let outcome = async {
        if transport.is_none() {
            *transport = Some(connect().await.map_err(crate::into_webpilot_error)?);
        }
        call_tool(transport.as_mut().expect("just connected"), name, &args).await
    }
    .await;

    // A dead transport (Chrome exited, socket dropped) must not poison the rest
    // of a long-lived MCP session: drop it on an infra-level failure (exit 3 —
    // ConnectionLost / BridgeUnavailable / VersionMismatch) so the next tool
    // call reopens a fresh one. Re-detecting a version mismatch is harmless.
    if outcome.as_ref().is_err_and(|e| e.exit_code() == 3) {
        *transport = None;
    }

    let result = match outcome {
        Ok(content) => json!({ "content": content, "isError": false }),
        Err(e) => json!({
            "content": [{ "type": "text", "text": format!("{e}") }],
            "isError": true,
        }),
    };
    ok_reply(id, result)
}

/// Execute one tool and return its MCP content blocks.
async fn call_tool<T: Transport>(
    transport: &mut T,
    name: &str,
    args: &Value,
) -> std::result::Result<Vec<Value>, WebPilotError> {
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
            let output = capture::run(
                transport,
                capture::CaptureArgs {
                    include: vec![CaptureField::Screenshot],
                    url: None,
                    opts: CaptureOpts::default(),
                },
            )
            .await
            .map_err(crate::into_webpilot_error)?;
            return screenshot_content(&output);
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
        "browser_wait" => {
            // Absent → default. Present-but-malformed (string, float, negative,
            // or zero) is a typed error, not a silent fall-back to the default —
            // a wait the caller asked for must not run for a duration they did
            // not specify.
            let timeout_ms =
                match args.get("timeout_ms") {
                    None | Some(Value::Null) => 10_000,
                    Some(v) => v.as_u64().filter(|&n| n > 0).ok_or_else(|| {
                        WebPilotError::InvalidArgument {
                            detail: "timeout_ms must be a positive integer (milliseconds)".into(),
                        }
                    })?,
                };
            // Reuse WaitCondition's tagged deserialization; keep only its
            // fields so the extra `timeout_ms` can't trip a stricter schema.
            let mut cond = serde_json::Map::new();
            if let Some(until) = args.get("until") {
                cond.insert("until".into(), until.clone());
            }
            if let Some(value) = args.get("value") {
                cond.insert("value".into(), value.clone());
            }
            let condition = serde_json::from_value(Value::Object(cond)).map_err(|e| {
                WebPilotError::InvalidArgument {
                    detail: format!("invalid wait condition: {e}"),
                }
            })?;
            // Pass the millisecond timeout straight through — routing it through
            // the seconds-based `WaitArgs` would round a sub-second request up to
            // a whole second.
            wait::dispatch(transport, condition, timeout_ms).await
        }
        other => {
            return Err(WebPilotError::InvalidArgument {
                detail: format!("unknown tool: {other}"),
            });
        }
    };
    let output = output.map_err(crate::into_webpilot_error)?;
    Ok(vec![
        json!({ "type": "text", "text": output.to_agent_text() }),
    ])
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

/// An MCP image block (plus the saved path as text) from a screenshot capture.
/// The host receives the actual pixels — a bare filesystem path would be
/// useless to a remote or sandboxed client.
fn screenshot_content(output: &CommandOutput) -> std::result::Result<Vec<Value>, WebPilotError> {
    let CommandOutput::Data { json: data, .. } = output else {
        return Err(WebPilotError::Other {
            detail: "screenshot produced no artefact".into(),
        });
    };
    let Some(path) = data.get("screenshot_path").and_then(Value::as_str) else {
        let detail = data
            .get("screenshot_error")
            .and_then(Value::as_str)
            .unwrap_or("screenshot produced no artefact");
        return Err(WebPilotError::Other {
            detail: detail.to_string(),
        });
    };
    let bytes = std::fs::read(path).map_err(|e| WebPilotError::Other {
        detail: format!("failed to read screenshot {path}: {e}"),
    })?;
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
    Ok(vec![
        json!({ "type": "image", "data": encoded, "mimeType": "image/png" }),
        json!({ "type": "text", "text": format!("Screenshot: {path}") }),
    ])
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
            "description": "Capture a screenshot of the current page; returns the image and its saved path.",
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
    fn initialize_advertises_the_supported_protocol_version() {
        // The server implements exactly one revision; per the MCP lifecycle it
        // must answer with a version it supports (never echo an unknown one)
        // and let the client decide.
        let r = initialize_result();
        assert_eq!(r["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(r["serverInfo"]["name"], "webpilot");
        assert!(r["capabilities"]["tools"].is_object());
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
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "{} schema",
                tool["name"]
            );
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
