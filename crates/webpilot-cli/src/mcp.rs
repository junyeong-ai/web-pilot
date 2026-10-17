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

use crate::commands::{action, capture, eval, find, frame, tab, wait};
use crate::output::CommandOutput;
use crate::transport::{IpcTransport, LocalTransport, Transport};

/// The MCP protocol revisions this server implements, newest first. Per the
/// MCP lifecycle, `initialize` echoes the client's requested revision when it
/// is supported, and otherwise answers with the newest — the client decides
/// whether to continue. Both listed revisions describe this tools-only stdio
/// server identically; older revisions differ on the wire (they allow JSON-RPC
/// batching, which this server rejects), so they are not claimed.
const MAX_PENDING_REQUESTS: usize = 32;

const SUPPORTED_PROTOCOL_VERSIONS: [&str; 2] = ["2025-11-25", "2025-06-18"];

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
    run_io(
        connect,
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await
}

async fn write_reply<W: tokio::io::AsyncWrite + Unpin>(
    output: &mut W,
    reply: &Value,
) -> Result<()> {
    let mut bytes = serde_json::to_vec(reply).expect("JSON-RPC response serializes");
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    output.flush().await?;
    Ok(())
}

async fn run_io<T, F, Fut, R, W>(connect: F, mut reader: R, mut output: W) -> Result<()>
where
    T: Transport,
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
    R: AsyncBufRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin,
{
    let (input_tx, input_rx) = tokio::sync::mpsc::channel(MAX_PENDING_REQUESTS);
    let input_task = tokio::spawn(async move {
        loop {
            let frame = read_frame(&mut reader, 8 * 1024 * 1024).await;
            let done = matches!(frame, Ok(Frame::Eof) | Err(_));
            if input_tx.send(frame).await.is_err() || done {
                break;
            }
        }
    });
    let result = run_requests(connect, input_rx, &mut output).await;
    input_task.abort();
    result
}

async fn run_requests<T, F, Fut, W>(
    connect: F,
    mut input_rx: tokio::sync::mpsc::Receiver<Result<Frame>>,
    mut output: W,
) -> Result<()>
where
    T: Transport,
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut transport = None;
    let mut initialized = false;
    let mut pending = std::collections::VecDeque::new();
    let mut cancelled = std::collections::HashSet::new();
    let mut eof = false;
    let session = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    loop {
        let frame = match pending.pop_front() {
            Some(frame) => frame,
            None if eof => break,
            None => match input_rx.recv().await {
                Some(frame) => frame?,
                None => break,
            },
        };
        let line = match frame {
            Frame::Eof => {
                eof = true;
                continue;
            }
            Frame::OverCap => {
                write_reply(
                    &mut output,
                    &error_reply(Value::Null, -32700, "request exceeds size limit"),
                )
                .await?;
                continue;
            }
            Frame::InvalidUtf8 => {
                write_reply(
                    &mut output,
                    &error_reply(Value::Null, -32700, "invalid UTF-8"),
                )
                .await?;
                continue;
            }
            Frame::Line(line) if line.trim().is_empty() => continue,
            Frame::Line(line) => line,
        };
        let message: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let id = message.get("id").cloned();
        if id
            .as_ref()
            .is_some_and(|id| cancelled.remove(&id.to_string()))
        {
            continue;
        }
        let request_id = id.as_ref().map(|id| format!("{session}:{}", id));
        let is_wait =
            message["method"] == "tools/call" && message["params"]["name"] == "browser_wait";
        let mut cancel_wait = false;
        let mut suppress_reply = false;
        let reply = {
            let work = handle_line_with_request_id(
                &connect,
                &mut transport,
                &mut initialized,
                &line,
                request_id.clone(),
            );
            tokio::pin!(work);
            loop {
                tokio::select! {
                    result = &mut work => break result,
                    incoming = input_rx.recv(), if !eof => {
                        let frame = match incoming { Some(frame) => frame?, None => { eof = true; continue; } };
                        if matches!(frame, Frame::Eof) { eof = true; continue; }
                        if let Frame::Line(control) = &frame
                            && let Ok(control) = serde_json::from_str::<Value>(control) {
                                if control["jsonrpc"] == "2.0" && control["method"] == "ping" {
                                    if let Some(id) = control.get("id") {
                                        write_reply(&mut output, &ok_reply(id.clone(), json!({}))).await?;
                                    }
                                    continue;
                                }
                                if control["jsonrpc"] == "2.0" && control["method"] == "notifications/cancelled" && control.get("id").is_none() {
                                    if let Some(target) = control["params"].get("requestId") {
                                        if id.as_ref() == Some(target) {
                                            suppress_reply = true;
                                            if is_wait { cancel_wait = true; break None; }
                                        } else if pending.iter().any(|frame| matches!(frame, Frame::Line(line) if serde_json::from_str::<Value>(line).ok().is_some_and(|request| request.get("id") == Some(target)))) {
                                            cancelled.insert(target.to_string());
                                        }
                                    }
                                    continue;
                                }
                                if pending.len() >= MAX_PENDING_REQUESTS {
                                    if let Some(id) = control.get("id") {
                                        write_reply(&mut output, &error_reply(id.clone(), -32000, "request queue is full; request was not started")).await?;
                                    }
                                    continue;
                                }
                            }
                        if pending.len() < MAX_PENDING_REQUESTS { pending.push_back(frame); }
                    }
                }
            }
        };
        if cancel_wait && let (Some(transport), Some(request_id)) = (transport.as_mut(), request_id)
        {
            let _ = transport
                .send(webpilot::protocol::Command::WaitCancel { request_id })
                .await;
        }
        if !suppress_reply && let Some(reply) = reply {
            write_reply(&mut output, &reply).await?;
        }
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
#[cfg(test)]
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
    handle_line_with_request_id(connect, transport, initialized, line, None).await
}

async fn handle_line_with_request_id<T, F, Fut>(
    connect: &F,
    transport: &mut Option<T>,
    initialized: &mut bool,
    line: &str,
    request_id: Option<String>,
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
    // A top-level array is a JSON-RPC batch. No supported MCP revision uses
    // batching (removed in 2025-06-18), so reject it with an invalid-request
    // rather than letting it fall through to the no-id notification path,
    // where the client would hang waiting for a batch response that never comes.
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
            Some(ok_reply(id, initialize_result(&params)))
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
        "tools/call" => Some(tool_call_reply(connect, transport, id, &params, request_id).await),
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
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|v| SUPPORTED_PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
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

fn validate_tool_arguments(name: &str, args: &Value) -> std::result::Result<(), WebPilotError> {
    static VALIDATORS: std::sync::OnceLock<
        std::collections::HashMap<String, jsonschema::Validator>,
    > = std::sync::OnceLock::new();
    let validators = VALIDATORS.get_or_init(|| {
        tool_specs()
            .as_array()
            .expect("tool list")
            .iter()
            .map(|tool| {
                (
                    tool["name"].as_str().expect("tool name").to_owned(),
                    jsonschema::validator_for(&tool["inputSchema"]).expect("valid tool schema"),
                )
            })
            .collect()
    });
    validators[name]
        .validate(args)
        .map_err(|error| WebPilotError::InvalidArgument {
            detail: format!(
                "invalid argument for {name} at {} (schema {})",
                error.instance_path(),
                error.schema_path()
            ),
        })
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
    request_id: Option<String>,
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

    if let Err(error) = validate_tool_arguments(name, &args) {
        return ok_reply(
            id,
            json!({"content":[{"type":"text", "text":error.to_string()}], "isError":true, "structuredContent":error.to_wire()}),
        );
    }

    let outcome = async {
        if transport.is_none() {
            *transport = Some(connect().await.map_err(crate::into_webpilot_error)?);
        }
        call_tool(
            transport.as_mut().expect("just connected"),
            name,
            &args,
            request_id,
        )
        .await
    }
    .await;

    // A dead transport (Chrome exited, socket dropped) must not poison the rest
    // of a long-lived MCP session: drop it on an infra-level failure (exit 3 —
    // ConnectionLost / BridgeUnavailable / VersionMismatch) so the next tool
    // call reopens a fresh one. Re-detecting a version mismatch is harmless.
    if outcome
        .as_ref()
        .is_err_and(|e| e.exit_code() == 3 || matches!(e, WebPilotError::OutcomeUnknown { .. }))
    {
        *transport = None;
    }

    let result = match outcome {
        Ok(result) => result,
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
    request_id: Option<String>,
) -> std::result::Result<Value, WebPilotError> {
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
            let include = args
                .get("include")
                .map(|value| serde_json::from_value::<Vec<CaptureField>>(value.clone()))
                .transpose()
                .map_err(|e| WebPilotError::InvalidArgument {
                    detail: e.to_string(),
                })?
                .unwrap_or_else(|| vec![CaptureField::Dom]);
            if include.is_empty()
                || include
                    .iter()
                    .any(|field| !matches!(field, CaptureField::Dom | CaptureField::Text))
            {
                return Err(WebPilotError::InvalidArgument {
                    detail: "snapshot include must contain dom or text".into(),
                });
            }
            capture::run(
                transport,
                capture::CaptureArgs {
                    include,
                    url: None,
                    opts: CaptureOpts::default(),
                },
            )
            .await
        }
        "browser_screenshot" => {
            let opts = CaptureOpts {
                full_page: bool_arg(args, "full_page")?,
                annotate: bool_arg(args, "annotate")?,
                ..CaptureOpts::default()
            };
            let output = capture::run(
                transport,
                capture::CaptureArgs {
                    include: vec![CaptureField::Screenshot],
                    url: None,
                    opts,
                },
            )
            .await
            .map_err(crate::into_webpilot_error)?;
            return Ok(
                json!({"content": screenshot_content(&output)?, "structuredContent": output.structured_content(), "isError": false}),
            );
        }
        "browser_click" | "browser_type" | "browser_press_key" | "browser_scroll"
        | "browser_scroll_to" | "browser_hover" | "browser_focus" | "browser_select"
        | "browser_upload" | "browser_drag" | "browser_back" | "browser_forward"
        | "browser_reload" => {
            let action = build_action(name, args)?;
            action::run(
                transport,
                action::ActionArgs {
                    action,
                    capture: args
                        .get("capture")
                        .map(|_| bool_arg(args, "capture"))
                        .transpose()?
                        .unwrap_or(true),
                    snapshot_id: args
                        .get("snapshot_id")
                        .map(|_| str_arg(args, "snapshot_id"))
                        .transpose()?,
                },
            )
            .await
        }
        "browser_find" => {
            let find_args: find::FindArgs = serde_json::from_value(args.clone()).map_err(|e| {
                WebPilotError::InvalidArgument {
                    detail: format!("invalid arguments for browser_find: {e}"),
                }
            })?;
            find::run(transport, find_args).await
        }
        "browser_tabs" => {
            let command = match str_arg(args, "action")?.as_str() {
                "list" => None,
                "new" => Some(tab::TabCommand::New {
                    url: str_arg(args, "url")?,
                }),
                "switch" => Some(tab::TabCommand::Switch {
                    tab_id: str_arg(args, "tab_id")?,
                }),
                "close" => Some(tab::TabCommand::Close {
                    tab_id: str_arg(args, "tab_id")?,
                }),
                "find" => Some(tab::TabCommand::Find {
                    url: str_arg(args, "url")?,
                }),
                other => {
                    return Err(WebPilotError::InvalidArgument {
                        detail: format!(
                            "unknown tabs action: {other} (expected list, new, switch, close, or find)"
                        ),
                    });
                }
            };
            tab::run(transport, tab::TabArgs { command }).await
        }
        // `FrameCommand::Find` (JS-predicate switching) stays CLI-only: the
        // curated MCP surface addresses frames by name or URL pattern.
        "browser_frame" => {
            let command = match str_arg(args, "action")?.as_str() {
                "list" => None,
                "switch" => Some(frame::FrameCommand::Switch {
                    name: str_arg(args, "name")?,
                }),
                "url" => Some(frame::FrameCommand::Url {
                    pattern: str_arg(args, "pattern")?,
                }),
                "main" => Some(frame::FrameCommand::Main),
                other => {
                    return Err(WebPilotError::InvalidArgument {
                        detail: format!(
                            "unknown frame action: {other} (expected list, switch, url, or main)"
                        ),
                    });
                }
            };
            frame::run(transport, frame::FrameArgs { command }).await
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
                    None => 10_000,
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
            wait::dispatch(transport, condition, timeout_ms, request_id).await
        }
        other => {
            return Err(WebPilotError::InvalidArgument {
                detail: format!("unknown tool: {other}"),
            });
        }
    };
    let output = output.map_err(crate::into_webpilot_error)?;
    Ok(json!({
        "content": [{ "type": "text", "text": output.to_agent_text() }],
        "structuredContent": output.structured_content(),
        "isError": false,
    }))
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
        "browser_scroll_to" => "scroll_to",
        "browser_hover" => "hover",
        "browser_focus" => "focus",
        "browser_select" => "select",
        "browser_upload" => "upload",
        "browser_drag" => "drag",
        "browser_back" => "back",
        "browser_forward" => "forward",
        "browser_reload" => "reload",
        _ => unreachable!("build_action only called for action tools"),
    };
    let mut obj = args.as_object().cloned().unwrap_or_default();
    obj.remove("snapshot_id");
    obj.remove("capture");
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

/// An optional boolean argument: absent (or null) is `false`; present but
/// non-boolean is a typed error, never a silent fall-back — an option the
/// caller set must not be dropped.
fn bool_arg(args: &Value, key: &str) -> std::result::Result<bool, WebPilotError> {
    match args.get(key) {
        None => Ok(false),
        Some(v) => v.as_bool().ok_or_else(|| WebPilotError::InvalidArgument {
            detail: format!("{key} must be a boolean"),
        }),
    }
}

/// The exposed tool surface, with JSON Schemas. Curated to the agent loop —
/// observe (snapshot/screenshot), act (the `Action` surface), orient (find /
/// tabs / frame), synchronize (wait) — plus `eval` as the escape hatch; each
/// acting tool auto-captures the resulting DOM so the model always gets a
/// fresh snapshot back. Every schema sets `additionalProperties: false`: the
/// schema is the argument contract, never a hint.
///
/// `annotations` carry only hints that differ from the spec defaults
/// (readOnlyHint=false, destructiveHint=true, idempotentHint=false,
/// openWorldHint=true). On the open web any page handler may commit changes,
/// so interacting tools keep the conservative defaults; only viewport/routing
/// tools (scroll, hover, focus, frame) are marked non-destructive.
fn tool_specs() -> &'static Value {
    static SPECS: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    SPECS.get_or_init(build_tool_specs)
}

fn build_tool_specs() -> Value {
    let index = json!({
        "type": "integer",
        "minimum": 1,
        "description": "Element index from the most recent snapshot's [N] markers.",
    });
    let modifiers = json!({
        "type": "object",
        "description": "Optional modifier keys held during the input.",
        "properties": {
            "ctrl": { "type": "boolean" },
            "shift": { "type": "boolean" },
            "alt": { "type": "boolean" },
            "meta": { "type": "boolean" },
        },
        "additionalProperties": false,
    });
    let mut specs = json!([
        {
            "name": "browser_navigate",
            "title": "Navigate to URL",
            "description": "Navigate to a URL and return the page's interactive-element snapshot.",
            "inputSchema": {
                "type": "object",
                "properties": { "url": { "type": "string", "description": "Absolute URL to load." } },
                "required": ["url"],
                "additionalProperties": false,
            },
        },
        {
            "name": "browser_snapshot",
            "title": "Page snapshot",
            "description": "Capture interactive elements (dom), visible text, or both. DOM results include the snapshot_id required by indexed actions.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "browser_screenshot",
            "title": "Screenshot",
            "description": "Capture a screenshot of the current page; returns the image and its saved path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "full_page": { "type": "boolean", "description": "Capture the entire scrollable area instead of the viewport.", "default": false },
                    "annotate": { "type": "boolean", "description": "Draw the snapshot's [N] labels on interactive elements (viewport only; cannot be combined with full_page).", "default": false },
                },
                "additionalProperties": false,
            },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "browser_click",
            "title": "Click element",
            "description": "Click the observed element using native pointer input. Requires its snapshot_id. Browser and platform modifier behavior applies.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index": index,
                    "modifiers": modifiers,
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
            "title": "Type text",
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
            "title": "Press key",
            "description": "Press a key (e.g. Enter, Tab, Escape, ArrowDown) on the focused element.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "Key name, e.g. \"Enter\" or a single character." },
                    "modifiers": modifiers,
                },
                "required": ["key"],
                "additionalProperties": false,
            },
        },
        {
            "name": "browser_scroll",
            "title": "Scroll page",
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
            "annotations": { "destructiveHint": false },
        },
        {
            "name": "browser_scroll_to",
            "title": "Scroll to element",
            "description": "Scroll until the element at the given index is in view, then return the updated snapshot.",
            "inputSchema": {
                "type": "object",
                "properties": { "index": index },
                "required": ["index"],
                "additionalProperties": false,
            },
            "annotations": { "destructiveHint": false, "idempotentHint": true },
        },
        {
            "name": "browser_hover",
            "title": "Hover element",
            "description": "Hover over the element at the given index (triggers hover menus and tooltips), then return the updated snapshot.",
            "inputSchema": {
                "type": "object",
                "properties": { "index": index },
                "required": ["index"],
                "additionalProperties": false,
            },
            "annotations": { "destructiveHint": false, "idempotentHint": true },
        },
        {
            "name": "browser_focus",
            "title": "Focus element",
            "description": "Focus the element at the given index, then return the updated snapshot.",
            "inputSchema": {
                "type": "object",
                "properties": { "index": index },
                "required": ["index"],
                "additionalProperties": false,
            },
            "annotations": { "destructiveHint": false, "idempotentHint": true },
        },
        {
            "name": "browser_select",
            "title": "Select option",
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
            "name": "browser_upload",
            "title": "Upload file",
            "description": "Attach a file to the <input type=file> element at the given index, then return the updated snapshot.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "index": index,
                    "path": { "type": "string", "description": "Path of the file to upload, on the machine running WebPilot." },
                },
                "required": ["index", "path"],
                "additionalProperties": false,
            },
        },
        {
            "name": "browser_drag",
            "title": "Drag element",
            "description": "Drag one element to another's position, then return the updated snapshot.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "integer", "minimum": 1, "description": "Index of the element to drag." },
                    "target": { "type": "integer", "minimum": 1, "description": "Index of the element to drop onto." },
                    "steps": { "type": "integer", "minimum": 1, "description": "Intermediate mouse-move steps.", "default": 5 },
                },
                "required": ["source", "target"],
                "additionalProperties": false,
            },
        },
        {
            "name": "browser_back",
            "title": "History back",
            "description": "Go back one entry in the tab's history, then return the updated snapshot.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        },
        {
            "name": "browser_forward",
            "title": "History forward",
            "description": "Go forward one entry in the tab's history, then return the updated snapshot.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        },
        {
            "name": "browser_reload",
            "title": "Reload page",
            "description": "Reload the current page, then return the updated snapshot.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        },
        {
            "name": "browser_find",
            "title": "Find elements",
            "description": "Find interactive elements by semantic filters (role, visible text, label, placeholder, tag) and list them with their snapshot indices. With click or fill, also acts on the match — the filter must then match exactly one element.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "role": { "type": "string", "description": "ARIA role or input type, e.g. button, link, textbox." },
                    "text": { "type": "string", "description": "Substring of the element's visible text." },
                    "label": { "type": "string", "description": "Substring of the element's label." },
                    "placeholder": { "type": "string", "description": "Substring of the element's placeholder." },
                    "tag": { "type": "string", "description": "Exact tag name, e.g. input, button." },
                    "click": { "type": "boolean", "description": "Click the single match. Mutually exclusive with fill.", "default": false },
                    "fill": { "type": "string", "description": "Replace the single match's value with this text. Mutually exclusive with click." },
                },
                "additionalProperties": false,
            },
        },
        {
            "name": "browser_tabs",
            "title": "Manage tabs",
            "description": "Manage browser tabs: list them, open a URL in a new tab, switch to or close a tab by id, or find (and switch to) the single tab matching a *-glob URL pattern.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "new", "switch", "close", "find"] },
                    "tab_id": { "type": "string", "description": "Tab id from `list` (required for switch/close)." },
                    "url": { "type": "string", "description": "URL to open (new) or *-glob pattern to match (find)." },
                },
                "required": ["action"],
                "additionalProperties": false,
            },
        },
        {
            "name": "browser_frame",
            "title": "Switch frame",
            "description": "Route subsequent commands to an iframe: list frames, switch by frame name or *-glob URL pattern, or return to the main frame. The active frame persists until switched again.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "switch", "url", "main"] },
                    "name": { "type": "string", "description": "Frame name (required for switch)." },
                    "pattern": { "type": "string", "description": "*-glob URL pattern matching exactly one frame (required for url)." },
                },
                "required": ["action"],
                "additionalProperties": false,
            },
            "annotations": { "destructiveHint": false, "idempotentHint": true },
        },
        {
            "name": "browser_eval",
            "title": "Evaluate JavaScript",
            "description": "Evaluate JavaScript in the page (MAIN world) and return the JSON-serialized result.",
            "inputSchema": {
                "type": "object",
                "properties": { "code": { "type": "string", "description": "JavaScript expression or statements." } },
                "required": ["code"],
                "additionalProperties": false,
            },
        },
        {
            "name": "browser_wait",
            "title": "Wait for condition",
            "description": "Wait for a condition before continuing.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "until": { "type": "string", "enum": ["selector", "text", "navigation", "idle"] },
                    "value": { "type": "string", "description": "Selector or text (required for until=selector|text)." },
                    "timeout_ms": { "type": "integer", "minimum": 1, "default": 10000 },
                },
                "required": ["until"],
                "additionalProperties": false,
            },
            "annotations": { "readOnlyHint": true },
        },
    ]);
    for tool in specs.as_array_mut().expect("tool list") {
        let name = tool["name"].as_str().expect("tool name").to_owned();
        let properties = tool["inputSchema"]["properties"]
            .as_object_mut()
            .expect("tool properties");
        if name == "browser_snapshot" {
            properties.insert("include".into(), json!({"type":"array", "items":{"type":"string", "enum":["dom","text"]}, "minItems":1, "default":["dom"]}));
        }
        if name.starts_with("browser_")
            && matches!(
                name.as_str(),
                "browser_click"
                    | "browser_type"
                    | "browser_press_key"
                    | "browser_scroll"
                    | "browser_scroll_to"
                    | "browser_hover"
                    | "browser_focus"
                    | "browser_select"
                    | "browser_upload"
                    | "browser_drag"
                    | "browser_back"
                    | "browser_forward"
                    | "browser_reload"
            )
        {
            let indexed = properties.contains_key("index") || properties.contains_key("source");
            properties.insert("capture".into(), json!({"type":"boolean", "default":true, "description":"Return a new DOM snapshot after the action."}));
            if indexed {
                properties.insert("snapshot_id".into(), json!({"type":"string", "description":"Snapshot identifier returned with the observed elements."}));
                tool["inputSchema"]["required"]
                    .as_array_mut()
                    .expect("indexed tool requirements")
                    .push(json!("snapshot_id"));
            }
        }
    }
    specs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn control_messages_remain_responsive_and_cancel_waits() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use webpilot::protocol::{Command, ResponseData};
        struct WaitingTransport {
            started: Arc<tokio::sync::Notify>,
            cancelled: Arc<AtomicUsize>,
            release: Arc<tokio::sync::Notify>,
            finished: Arc<AtomicUsize>,
        }
        impl Transport for WaitingTransport {
            async fn send(&mut self, command: Command) -> Result<ResponseData> {
                match command {
                    Command::Wait { .. } => {
                        self.started.notify_one();
                        std::future::pending().await
                    }
                    Command::WaitCancel { .. } => {
                        self.cancelled.fetch_add(1, Ordering::SeqCst);
                        Ok(ResponseData::Wait {
                            success: true,
                            error: None,
                        })
                    }
                    Command::Eval { .. } => {
                        self.started.notify_one();
                        self.release.notified().await;
                        self.finished.fetch_add(1, Ordering::SeqCst);
                        Ok(ResponseData::Eval {
                            success: true,
                            result: Some("1".into()),
                            error: None,
                        })
                    }
                    Command::TabList => Ok(ResponseData::Tabs { tabs: vec![] }),
                    _ => panic!("unexpected browser operation"),
                }
            }
        }
        let started = Arc::new(tokio::sync::Notify::new());
        let cancelled = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let finished = Arc::new(AtomicUsize::new(0));
        let (client, server) = tokio::io::duplex(8192);
        let (reader, mut writer) = tokio::io::split(server);
        let started_server = started.clone();
        let cancelled_server = cancelled.clone();
        let release_server = release.clone();
        let finished_server = finished.clone();
        let server = tokio::spawn(async move {
            run_io(
                move || {
                    let started = started_server.clone();
                    let cancelled = cancelled_server.clone();
                    let release = release_server.clone();
                    let finished = finished_server.clone();
                    async move {
                        Ok(WaitingTransport {
                            started,
                            cancelled,
                            release,
                            finished,
                        })
                    }
                },
                BufReader::new(reader),
                &mut writer,
            )
            .await
        });
        let (reader, mut writer) = tokio::io::split(client);
        let mut reader = BufReader::new(reader);
        async fn reply(reader: &mut (impl AsyncBufRead + Unpin)) -> Value {
            let mut line = String::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                reader.read_line(&mut line),
            )
            .await
            .unwrap()
            .unwrap();
            serde_json::from_str(&line).unwrap()
        }
        write_reply(
            &mut writer,
            &json!({"jsonrpc":"2.0", "id":0,"method":"initialize"}),
        )
        .await
        .unwrap();
        assert_eq!(reply(&mut reader).await["id"], 0);
        write_reply(&mut writer, &json!({"jsonrpc":"2.0", "id":1,"method":"tools/call", "params":{"name":"browser_wait","arguments":{"until":"idle"}}})).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        write_reply(
            &mut writer,
            &json!({"jsonrpc":"2.0", "id":2,"method":"ping"}),
        )
        .await
        .unwrap();
        assert_eq!(reply(&mut reader).await["id"], 2);
        write_reply(
            &mut writer,
            &json!({"jsonrpc":"2.0", "method":"notifications/cancelled","params":{"requestId":1}}),
        )
        .await
        .unwrap();
        write_reply(&mut writer, &json!({"jsonrpc":"2.0", "id":3,"method":"tools/call","params":{"name":"browser_tabs","arguments":{"action":"list"}}})).await.unwrap();
        let response = reply(&mut reader).await;
        assert_eq!(response["id"], 3);
        assert_eq!(response["result"]["structuredContent"]["value"], json!([]));
        assert_eq!(cancelled.load(Ordering::SeqCst), 1);
        write_reply(&mut writer, &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"browser_eval","arguments":{"code":"1"}}})).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        write_reply(
            &mut writer,
            &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":4}}),
        )
        .await
        .unwrap();
        write_reply(&mut writer, &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"browser_tabs","arguments":{"action":"list"}}})).await.unwrap();
        write_reply(
            &mut writer,
            &json!({"jsonrpc":"2.0","id":6,"method":"ping"}),
        )
        .await
        .unwrap();
        assert_eq!(reply(&mut reader).await["id"], 6);
        assert_eq!(finished.load(Ordering::SeqCst), 0);
        release.notify_one();
        assert_eq!(reply(&mut reader).await["id"], 5);
        assert_eq!(finished.load(Ordering::SeqCst), 1);
        writer.shutdown().await.unwrap();
        server.await.unwrap().unwrap();
    }

    #[test]
    fn initialize_negotiates_the_protocol_version() {
        // A supported requested revision is echoed back; an unsupported or
        // absent one gets the newest supported revision — the client decides
        // whether to continue.
        for v in SUPPORTED_PROTOCOL_VERSIONS {
            let r = initialize_result(&json!({ "protocolVersion": v }));
            assert_eq!(r["protocolVersion"], v);
        }
        let r = initialize_result(&json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(r["protocolVersion"], SUPPORTED_PROTOCOL_VERSIONS[0]);
        let r = initialize_result(&Value::Null);
        assert_eq!(r["protocolVersion"], SUPPORTED_PROTOCOL_VERSIONS[0]);
        assert_eq!(r["serverInfo"]["name"], "webpilot");
        assert!(r["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tool_specs_cover_the_agent_loop_with_strict_object_schemas() {
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
            "browser_screenshot",
            "browser_click",
            "browser_type",
            "browser_press_key",
            "browser_scroll",
            "browser_scroll_to",
            "browser_hover",
            "browser_focus",
            "browser_select",
            "browser_upload",
            "browser_drag",
            "browser_back",
            "browser_forward",
            "browser_reload",
            "browser_find",
            "browser_tabs",
            "browser_frame",
            "browser_eval",
            "browser_wait",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
        // Every schema is a strict object contract and every tool carries a
        // display title — uniformly, so no tool's arguments are ever a hint.
        for tool in specs.as_array().unwrap() {
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "{} schema",
                tool["name"]
            );
            assert_eq!(
                tool["inputSchema"]["additionalProperties"],
                json!(false),
                "{} schema must forbid unknown properties",
                tool["name"]
            );
            assert!(tool["title"].is_string(), "{} needs a title", tool["name"]);
        }
    }

    #[test]
    fn read_only_annotations_mark_exactly_the_observing_tools() {
        // readOnlyHint lets a host skip confirmation for pure observation.
        // `browser_find` is NOT read-only: its click/fill arguments act on the
        // page, and a static hint must cover the tool's whole capability.
        let specs = tool_specs();
        let read_only: Vec<&str> = specs
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["annotations"]["readOnlyHint"] == json!(true))
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            read_only,
            ["browser_snapshot", "browser_screenshot", "browser_wait"]
        );
    }

    #[test]
    fn action_tool_schemas_forbid_unknown_properties() {
        // Every tool whose handler deserializes a strict type
        // (`deny_unknown_fields` on `Action` / `FindArgs`) must advertise
        // `additionalProperties: false`, so a client learns an unknown
        // property is rejected from the schema, not only at runtime.
        let specs = tool_specs();
        let arr = specs.as_array().unwrap();
        for name in [
            "browser_click",
            "browser_type",
            "browser_press_key",
            "browser_scroll",
            "browser_scroll_to",
            "browser_hover",
            "browser_focus",
            "browser_select",
            "browser_upload",
            "browser_drag",
            "browser_find",
        ] {
            let tool = arr.iter().find(|t| t["name"] == name).unwrap();
            assert_eq!(
                tool["inputSchema"]["additionalProperties"],
                json!(false),
                "{name} schema must forbid unknown properties"
            );
        }
    }

    #[test]
    fn argument_validation_rejects_unknown_and_missing_fields() {
        assert!(validate_tool_arguments("browser_snapshot", &json!({"hard": true})).is_err());
        assert!(validate_tool_arguments("browser_click", &json!({"index": 1})).is_err());
        assert!(
            validate_tool_arguments("browser_click", &json!({"index": 1, "snapshot_id": "s"}))
                .is_ok()
        );
        assert!(validate_tool_arguments("browser_snapshot", &json!({"include":["text"]})).is_ok());
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
        // `drag` without `steps` → the shared default applies.
        let a = build_action("browser_drag", &json!({ "source": 1, "target": 2 })).unwrap();
        assert_eq!(
            a,
            Action::Drag {
                source: 1,
                target: 2,
                steps: 5,
            }
        );
        // A no-argument history tool builds its unit variant.
        let a = build_action("browser_back", &json!({})).unwrap();
        assert_eq!(a, Action::Back);
        // `upload` carries the server-local path through as a PathBuf.
        let a = build_action(
            "browser_upload",
            &json!({ "index": 2, "path": "/tmp/f.png" }),
        )
        .unwrap();
        assert!(matches!(a, Action::Upload { index: 2, .. }));
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
