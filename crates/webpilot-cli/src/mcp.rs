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
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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
    // not a request.
    const MAX_LINE_BYTES: u64 = 8 * 1024 * 1024;
    let mut transport: Option<T> = None;
    // MCP lifecycle: a `tools/*` call before `initialize` is rejected (-32002),
    // per the spec's initialize-then-operate order. Flipped true once an
    // `initialize` request is handled.
    let mut initialized = false;
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    loop {
        let reply = match read_frame(&mut reader, MAX_LINE_BYTES).await? {
            Frame::Eof => break,
            Frame::Line(line) if line.trim().is_empty() => continue,
            Frame::Line(line) => {
                // Isolate a panic in tool dispatch: an `unwrap`/`expect` reached
                // mid-handler must not unwind through the stdio loop and kill a
                // long-lived MCP session (every later tool call would get no
                // response). Catch it, reset the transport (its state may be
                // inconsistent past the unwind — the next call reopens), and
                // reply with a JSON-RPC internal error. The id is unrecoverable
                // past the panic, so null per spec.
                let fut = std::panic::AssertUnwindSafe(handle_line(
                    &connect,
                    &mut transport,
                    &mut initialized,
                    &line,
                ));
                match futures_util::FutureExt::catch_unwind(fut).await {
                    Ok(Some(reply)) => reply,
                    Ok(None) => continue, // a notification — no response
                    Err(_panic) => {
                        transport = None;
                        error_reply(
                            Value::Null,
                            -32603,
                            "internal error: tool dispatch panicked",
                        )
                    }
                }
            }
            Frame::OverCap => error_reply(Value::Null, -32700, "request exceeds size limit"),
            Frame::InvalidUtf8 => error_reply(Value::Null, -32700, "parse error: invalid UTF-8"),
        };
        let mut out = serde_json::to_string(&reply).expect("JSON-RPC reply serializes");
        out.push('\n');
        stdout.write_all(out.as_bytes()).await?;
        stdout.flush().await?;
    }
    Ok(())
}

/// One framing outcome from [`read_frame`]: a usable line, or one of the
/// malformed inputs the loop answers with a parse error, or end of stream.
#[cfg_attr(test, derive(Debug, PartialEq))]
enum Frame {
    Line(String),
    OverCap,
    InvalidUtf8,
    Eof,
}

/// Read one newline-delimited JSON-RPC frame, bounded at `max` bytes. The `take`
/// bound keeps one read from growing memory without limit. When a line exceeds
/// the cap, its remaining bytes are **drained through the terminating newline**
/// so the next read starts at a clean frame boundary — without this, the tail of
/// a single over-cap line would be parsed as a fresh request and desync every
/// frame after it (length-framed transports like native messaging and the IPC
/// host don't share this hazard; this newline-framed stdin stream must drain
/// explicitly).
async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R, max: u64) -> Result<Frame> {
    let mut buf = Vec::new();
    let n = (&mut *reader)
        .take(max + 1)
        .read_until(b'\n', &mut buf)
        .await?;
    if n == 0 {
        return Ok(Frame::Eof);
    }
    if buf.len() as u64 > max && buf.last() != Some(&b'\n') {
        // Drain the over-cap line through its terminating newline so the stream
        // resyncs at a clean frame instead of parsing this line's tail as a fresh
        // request. Each iteration awaits I/O and discards at most `max` bytes (the
        // buffer is cleared every time), so this neither busy-spins nor grows
        // memory: a finite over-cap line drains to its newline and the stream
        // resyncs (O(line length), no arbitrary cap to leave a misparsed tail),
        // and an infinite one — a client streaming bytes with no newline — is
        // drained until EOF, which is unavoidable and harmless: that one line IS
        // the whole stdin stream, so there is nothing else to process meanwhile.
        // (Length-framed transports — native messaging, the IPC host — consume an
        // exact byte count and so never face an unterminated frame.)
        while buf.last() != Some(&b'\n') {
            buf.clear();
            let drained = (&mut *reader).take(max).read_until(b'\n', &mut buf).await?;
            if drained == 0 {
                break; // EOF mid-line — nothing left to resync to
            }
        }
        return Ok(Frame::OverCap);
    }
    // Strict UTF-8: a JSON-RPC frame is UTF-8 by spec, so decode rather than
    // lossily coerce invalid bytes into U+FFFD and parse garbage.
    match String::from_utf8(buf) {
        Ok(line) => Ok(Frame::Line(line)),
        Err(_) => Ok(Frame::InvalidUtf8),
    }
}

/// Route one JSON-RPC message. Returns `None` only for a notification — a
/// request with NO `id` member at all. An explicit `id: null` is a request and
/// is answered (with a null id), distinct from an absent id.
async fn handle_line<T, F, Fut>(
    connect: &F,
    transport: &mut Option<T>,
    initialized: &mut bool,
    line: &str,
) -> Option<Value>
where
    T: Transport,
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        // JSON-RPC 2.0: a parse error is answered with a null id, never dropped.
        return Some(error_reply(Value::Null, -32700, "parse error"));
    };
    // A top-level array is a JSON-RPC batch. MCP (2025-06-18) does not use
    // batching, so reject it with an invalid-request rather than letting it fall
    // through to the no-id notification path, where the client would hang
    // waiting for a batch response that never comes.
    if msg.is_array() {
        return Some(error_reply(
            Value::Null,
            -32600,
            "invalid request: JSON-RPC batches are not supported",
        ));
    }
    // Validate the envelope BEFORE deciding notification-vs-request. A message
    // is only a true notification if it is otherwise well-formed but carries no
    // id; a malformed one (no `"jsonrpc": "2.0"`, no string `method`) must be
    // answered with -32600 — using null when it carries no id — not silently
    // dropped down the no-id path, where a client awaiting an answer would hang.
    // This is the same "never silently drop a malformed request" rule the batch
    // guard above applies. JSON-RPC 2.0 requires `"jsonrpc": "2.0"`.
    if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(error_reply(
            msg.get("id").cloned().unwrap_or(Value::Null),
            -32600,
            "invalid request: jsonrpc must be \"2.0\"",
        ));
    }
    // A string `method` is required; its absence is -32600 (invalid request),
    // distinct from -32601 (a well-formed call to a method that does not exist).
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        return Some(error_reply(
            msg.get("id").cloned().unwrap_or(Value::Null),
            -32600,
            "invalid request: missing method",
        ));
    };
    // Envelope is valid. Absent id now means a TRUE notification — process with
    // no response. Present id (including an explicit null) is a request to echo.
    let id = msg.get("id").cloned()?;
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    match method {
        // `initialize` is the lifecycle entry point — always allowed, and it
        // arms the gate for the operational methods below. `ping` is a liveness
        // check the spec allows at any time, before initialization included.
        "initialize" => {
            *initialized = true;
            Some(ok_reply(id, initialize_result()))
        }
        "ping" => Some(ok_reply(id, json!({}))),
        // Operational methods require initialization first (MCP lifecycle): a
        // `tools/*` before `initialize` is -32002, not served as if the
        // capability handshake had happened.
        "tools/list" | "tools/call" if !*initialized => Some(error_reply(
            id,
            -32002,
            "server not initialized — send `initialize` first",
        )),
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

/// The `-32602` reason a `tools/call` params object is structurally malformed,
/// or `None` when its shape is valid: `name` must be a string and `arguments`,
/// when present, an object. (An unknown tool NAME is also `-32602`, decided by
/// the separate `tool_exists` check; a structurally-valid call to a real tool
/// that then fails at execution — a bad argument value, element not found — is
/// an `isError` tool result, not a request error.)
fn tool_call_param_error(params: &Value) -> Option<&'static str> {
    if params.get("name").and_then(Value::as_str).is_none() {
        return Some("invalid params: `name` must be a string");
    }
    if params.get("arguments").is_some_and(|a| !a.is_object()) {
        return Some("invalid params: `arguments` must be an object");
    }
    None
}

/// Whether `name` is an advertised tool — checked against the same
/// `tool_specs` that `tools/list` serves, one source for both.
fn tool_exists(name: &str) -> bool {
    tool_specs()
        .as_array()
        .expect("tool_specs is an array (static shape)")
        .iter()
        .any(|t| t.get("name").and_then(Value::as_str) == Some(name))
}

/// A tool failure is reported as a successful JSON-RPC response carrying
/// `isError: true`, so the model sees the message and can react, per the MCP
/// tool-call contract. Malformed requests and unknown tool names use
/// JSON-RPC-level errors (-32602), per spec.
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
    // Honor the contract above: a malformed REQUEST is a JSON-RPC-level error,
    // not a tool-execution failure. A non-string `name` or a non-object
    // `arguments` would otherwise reach a tool and surface as a misleading
    // "missing <field>" `isError` result instead of `-32602 invalid params`.
    if let Some(reason) = tool_call_param_error(params) {
        return error_reply(id, -32602, reason);
    }
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .expect("validated by tool_call_param_error");
    // An unknown tool NAME is a protocol-level error per the MCP spec (-32602
    // with "Unknown tool"), not a tool-execution failure — and it is decided
    // BEFORE any transport opens, so a typo'd name can't launch Chrome. The
    // check reads the same `tool_specs` that `tools/list` serves, so it can
    // never drift from the advertised surface.
    if !tool_exists(name) {
        return error_reply(id, -32602, &format!("unknown tool: {name}"));
    }
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
        // The text block carries the agent-facing guidance (parity with the CLI's
        // human output); `structuredContent` carries the typed `{code, ...data}`
        // wire error (parity with the CLI's `--json`), so a client can branch on
        // `ElementNotFound` vs `Timeout` vs `PolicyDenied` instead of parsing prose.
        Err(e) => json!({
            "content": [{ "type": "text", "text": format!("{e}") }],
            "isError": true,
            "structuredContent": serde_json::to_value(e.to_wire())
                .expect("WireError serializes"),
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
    // Reuse the capture renderer so the text block carries the SAME identity lines
    // the CLI prints — `Page:`/`Title:` alongside `Screenshot:`. A screenshot has
    // no DOM footer, so without this an MCP client holding the image can't tell
    // which page (after a redirect, or a switched iframe) it actually shows.
    let text = crate::output::dom_extra_lines(data.as_object().ok_or(WebPilotError::Other {
        detail: "screenshot output was not an object".into(),
    })?)
    .join("\n");
    Ok(vec![
        json!({ "type": "image", "data": encoded, "mimeType": "image/png" }),
        json!({ "type": "text", "text": text }),
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
            "description": "Click the element at the given snapshot index, then return the updated snapshot. Modifier flags reach the page's own handlers; browser-level open-in-new-tab does not apply to a synthetic click — browser_navigate to the link's URL instead.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index": index,
                    "modifiers": {
                        "type": "object",
                        "description": "Optional modifier keys held during the click.",
                        "properties": {
                            "ctrl": { "type": "boolean" },
                            "shift": { "type": "boolean" },
                            "alt": { "type": "boolean" },
                            "meta": { "type": "boolean" },
                        },
                        "additionalProperties": false,
                    },
                },
                "required": ["index"],
                // The handler deserializes a strict `Action` (`deny_unknown_fields`),
                // so the schema advertises that an unknown property is rejected
                // rather than letting a client learn it only at runtime.
                "additionalProperties": false,
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
                "additionalProperties": false,
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
                        "additionalProperties": false,
                    },
                },
                "required": ["key"],
                "additionalProperties": false,
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
                "additionalProperties": false,
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
                "additionalProperties": false,
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
    fn action_tool_schemas_forbid_unknown_properties() {
        // Every tool whose handler deserializes a strict `Action`
        // (`deny_unknown_fields`) must advertise `additionalProperties: false`,
        // so a client learns an unknown property is rejected from the schema,
        // not only at runtime.
        let specs = tool_specs();
        let arr = specs.as_array().unwrap();
        for name in [
            "browser_click",
            "browser_type",
            "browser_press_key",
            "browser_scroll",
            "browser_select",
        ] {
            let tool = arr.iter().find(|t| t["name"] == name).unwrap();
            assert_eq!(
                tool["inputSchema"]["additionalProperties"],
                json!(false),
                "{name} schema must forbid unknown properties"
            );
        }
    }

    #[tokio::test]
    async fn tools_calls_require_initialize_first() {
        // MCP lifecycle: a `tools/*` before `initialize` is -32002 and opens no
        // transport; `initialize` arms the gate; `tools/list` then passes.
        let connect = || async { anyhow::Ok(crate::transport::IpcTransport::new()) };
        let mut transport: Option<crate::transport::IpcTransport> = None;
        let mut initialized = false;

        let call = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"browser_snapshot"}}"#;
        let reply = handle_line(&connect, &mut transport, &mut initialized, call)
            .await
            .unwrap();
        assert_eq!(reply["error"]["code"], json!(-32002), "{reply}");
        assert!(transport.is_none(), "no transport opens before initialize");
        assert!(!initialized);

        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let reply = handle_line(&connect, &mut transport, &mut initialized, init)
            .await
            .unwrap();
        assert!(reply["result"].is_object());
        assert!(initialized, "initialize arms the lifecycle gate");

        let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let reply = handle_line(&connect, &mut transport, &mut initialized, list)
            .await
            .unwrap();
        assert!(
            reply["result"]["tools"].is_array(),
            "tools/list passes the gate once initialized: {reply}"
        );
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
        // `click` with a modifiers object → the flags survive the MCP build
        // path into the typed Action (the schema advertises them; a silent
        // drop here would make the advertised field a lie).
        let a = build_action(
            "browser_click",
            &json!({ "index": 1, "modifiers": { "shift": true } }),
        )
        .unwrap();
        match a {
            Action::Click { index, modifiers } => {
                assert_eq!(index, 1);
                assert!(modifiers.shift && !modifiers.ctrl);
            }
            other => panic!("expected Click, got {other:?}"),
        }
    }

    #[test]
    fn build_action_rejects_wrong_types() {
        let err = build_action("browser_click", &json!({ "index": "not-a-number" }));
        assert!(matches!(err, Err(WebPilotError::InvalidArgument { .. })));
    }

    #[tokio::test]
    async fn read_frame_drains_an_over_cap_line_so_the_next_frame_resyncs() {
        use tokio::io::BufReader;
        // An over-cap line (longer than the cap, no newline within it) followed
        // by a valid frame: the over-cap line must report OverCap with its tail
        // drained through the newline, and the NEXT read must return the valid
        // frame cleanly — not the over-cap line's residue parsed as a request.
        let max = 8u64;
        let mut input = vec![b'X'; (max + 5) as usize]; // 13 bytes, no newline → over cap
        input.push(b'\n');
        input.extend_from_slice(b"ab\n");
        let mut reader = BufReader::new(input.as_slice());

        assert_eq!(
            read_frame(&mut reader, max).await.unwrap(),
            Frame::OverCap,
            "the >cap line is reported as OverCap"
        );
        assert_eq!(
            read_frame(&mut reader, max).await.unwrap(),
            Frame::Line("ab\n".into()),
            "the next read resyncs to the valid frame, not the drained residue"
        );
        assert_eq!(
            read_frame(&mut reader, max).await.unwrap(),
            Frame::Eof,
            "then end of stream"
        );
    }

    #[tokio::test]
    async fn read_frame_classifies_line_invalid_utf8_and_eof() {
        use tokio::io::BufReader;
        let data = b"hello\n\xff\xfe\nbye";
        let mut reader = BufReader::new(&data[..]);
        assert_eq!(
            read_frame(&mut reader, 1024).await.unwrap(),
            Frame::Line("hello\n".into())
        );
        assert_eq!(
            read_frame(&mut reader, 1024).await.unwrap(),
            Frame::InvalidUtf8
        );
        // A final unterminated but in-cap line is still a usable line.
        assert_eq!(
            read_frame(&mut reader, 1024).await.unwrap(),
            Frame::Line("bye".into())
        );
        assert_eq!(read_frame(&mut reader, 1024).await.unwrap(), Frame::Eof);
    }

    #[tokio::test]
    async fn read_frame_drains_a_long_unterminated_line_to_eof() {
        use tokio::io::BufReader;
        // A line far longer than the cap, with no newline, then EOF: read_frame
        // drains it fully (in `max`-sized chunks, awaiting I/O each time) and
        // terminates on EOF — one OverCap, then Eof — never spinning and never
        // leaving an undrained tail a later read would misparse as a request.
        let max = 4u64;
        let data = vec![b'X'; 600];
        let mut reader = BufReader::new(data.as_slice());
        assert_eq!(
            read_frame(&mut reader, max).await.unwrap(),
            Frame::OverCap,
            "the unterminated line is reported once as OverCap"
        );
        assert_eq!(
            read_frame(&mut reader, max).await.unwrap(),
            Frame::Eof,
            "the drain consumed to EOF, so the next read is end-of-stream"
        );
    }

    #[test]
    fn tool_call_param_error_flags_malformed_shape_only() {
        // Valid shapes carry no param error.
        assert_eq!(
            tool_call_param_error(&json!({ "name": "browser_snapshot" })),
            None
        );
        assert_eq!(
            tool_call_param_error(&json!({ "name": "browser_eval", "arguments": { "code": "1" } })),
            None
        );
        // A well-formed but UNKNOWN tool name is NOT a shape error — the
        // separate `tool_exists` check rejects it at protocol level (-32602)
        // before any transport opens.
        assert_eq!(
            tool_call_param_error(&json!({ "name": "browser_unknown" })),
            None
        );
        assert!(
            !tool_exists("browser_unknown"),
            "an unadvertised name must fail the existence check"
        );
        assert!(
            tool_exists("browser_snapshot"),
            "an advertised tool must pass the existence check"
        );
        // Structurally malformed requests are -32602 param errors.
        assert!(tool_call_param_error(&json!({})).is_some(), "missing name");
        assert!(
            tool_call_param_error(&json!({ "name": 5 })).is_some(),
            "non-string name"
        );
        assert!(
            tool_call_param_error(&json!({ "name": "x", "arguments": "nope" })).is_some(),
            "non-object arguments"
        );
        assert!(
            tool_call_param_error(&json!({ "name": "x", "arguments": [1, 2] })).is_some(),
            "array arguments"
        );
    }
}
