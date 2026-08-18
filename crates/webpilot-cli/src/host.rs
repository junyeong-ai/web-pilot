//! Native Messaging Host mode.
//!
//! Chrome launches the binary with `chrome-extension://...` as the first arg.
//! `main` recognises that signature and dispatches here. The host owns three
//! background tasks:
//!   1. NM stdin reader  — receives messages from Extension.
//!   2. NM stdout writer — sends messages to Extension.
//!   3. IPC listener     — accepts CLI requests, forwards them, awaits replies.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{Mutex, mpsc, oneshot};

use webpilot::{WebPilotError, dirs, ipc, native_messaging};

use crate::assets;

type Pending = std::collections::HashMap<u32, oneshot::Sender<serde_json::Value>>;

/// Result of comparing the installed extension's reported version against the
/// version bundled into this binary. Driven by the extension's connect-time
/// hello and every keepalive Ping.
#[derive(Clone)]
enum GateState {
    /// No version-bearing Ping seen yet (host just started). Permissive — the
    /// hello arrives within the first round-trip, long before a CLI request in
    /// practice, and an unknown version is not yet a known skew.
    Unknown,
    /// Reported version equals the bundled version.
    Matched,
    /// Reported version differs (or a Ping carried no version at all, meaning a
    /// pre-handshake extension). The string is what the extension reported
    /// (empty = none). CLI requests are rejected with `VersionMismatch`.
    Mismatch(String),
}

type VersionGate = Arc<std::sync::Mutex<GateState>>;

/// Bound on the host log before it rotates. The host records session lifecycle
/// and failures — a handful of lines per session — so this holds a long history,
/// and two files bound the directory for good.
const LOG_ROTATE_BYTES: u64 = 1024 * 1024;

/// Send the host's tracing output to a file.
///
/// Chrome spawns this process and owns its stdio, so `stderr` reaches nobody:
/// a browser-mode session that misbehaves leaves no trace of what the host saw.
/// The CLI keeps stderr, where the caller does capture it.
///
/// Falls back to stderr if the file cannot be opened — a host that cannot log is
/// still a host that must serve.
fn init_logging() {
    let builder = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false);
    match open_log() {
        Some(file) => {
            let _ = builder
                .with_writer(Arc::new(file))
                .with_ansi(false)
                .try_init();
        }
        None => {
            let _ = builder.with_writer(std::io::stderr).try_init();
        }
    }
}

/// The log to append to, rotating first when the current one is full.
///
/// Rotation renames rather than truncates: a host still writing to the old
/// inode keeps landing in `host.log.1` instead of leaving a hole in the new
/// file, and the previous session's account survives — which is the one a report
/// about a session that has already ended needs.
fn open_log() -> Option<std::fs::File> {
    open_log_at(&dirs::host_log_path())
}

fn open_log_at(path: &std::path::Path) -> Option<std::fs::File> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() >= LOG_ROTATE_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

pub async fn run_host() -> anyhow::Result<()> {
    init_logging();

    tracing::info!("WebPilot host starting");

    // Fail loudly on a malformed `config.toml` rather than serving a long-lived
    // host with settings the operator believes are in effect but aren't.
    webpilot::settings::init().map_err(|e| anyhow::anyhow!(e))?;

    let pending: Arc<Mutex<Pending>> = Arc::new(Mutex::new(Pending::new()));
    let (nm_tx, nm_rx) = mpsc::channel::<serde_json::Value>(32);
    let version_gate: VersionGate = Arc::new(std::sync::Mutex::new(GateState::Unknown));

    let ipc_listener = ipc::start_server().await?;

    let nm_writer_handle = spawn_nm_writer(nm_rx);
    let nm_reader_handle = spawn_nm_reader(pending.clone(), nm_tx.clone(), version_gate.clone());
    // The host owns the id space facing Chrome: each forwarded request gets a
    // host-unique id, so concurrent CLI processes (which each start their own
    // counter) can never collide in `pending`.
    let ids = Arc::new(AtomicU32::new(1));
    let ipc_handle = tokio::spawn(handle_ipc_connections(
        ipc_listener,
        nm_tx.clone(),
        pending.clone(),
        ids,
        version_gate,
    ));

    tracing::info!("Host ready");

    // The reader exits when Chrome disconnects — the host's lifetime IS
    // Chrome's.
    let _ = nm_reader_handle.await;
    drop((nm_tx, nm_writer_handle, ipc_handle));
    // Exit the process, not a graceful teardown: the NM writer is a BLOCKING
    // task that ends only when every `nm_tx` clone drops, and the detached
    // per-connection IPC tasks each hold one — so awaiting the writer here
    // pended forever (the accept task alone was enough) and every Chrome
    // restart leaked an orphan host. Nothing we hold outlives Chrome
    // usefully: the socket is deliberately NOT unlinked (a successor host
    // rebinds the fixed per-user path at bind time in `ipc::start_server`;
    // unlinking here would delete a live successor's socket), and a CLI
    // mid-request observes the closing socket as `ConnectionLost` — exactly
    // "Chrome died mid-command".
    tracing::info!("Chrome disconnected — host exiting");
    std::process::exit(0);
}

fn spawn_nm_writer(mut rx: mpsc::Receiver<serde_json::Value>) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        while let Some(msg) = rx.blocking_recv() {
            let mut stdout = std::io::stdout().lock();
            match native_messaging::write_message(&mut stdout, &msg) {
                Ok(()) => {}
                // Backstop only: forwarded commands are size-checked before
                // enqueue (the CLI gets a typed error), so this fires only for
                // a host-originated message that somehow exceeds the frame. The
                // payload is rejected before any byte reaches the stream, so the
                // pipe stays intact — skip it and keep serving. Only a real IO
                // failure (broken pipe) ends the writer.
                Err(e @ native_messaging::NmError::TooLarge(_)) => {
                    tracing::error!("dropping oversized NM message: {e}");
                }
                Err(e) => {
                    tracing::error!("NM write error: {e}");
                    break;
                }
            }
        }
    })
}

fn spawn_nm_reader(
    pending: Arc<Mutex<Pending>>,
    nm_tx: mpsc::Sender<serde_json::Value>,
    version_gate: VersionGate,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut stdin = std::io::stdin().lock();
        loop {
            match native_messaging::read_message(&mut stdin) {
                Ok(msg) => {
                    if let Err(e) = process_nm_message(msg, &pending, &nm_tx, &version_gate) {
                        tracing::error!("NM message handling failed: {e}");
                    }
                }
                Err(native_messaging::NmError::Eof) => {
                    tracing::info!("Chrome disconnected");
                    break;
                }
                Err(e) => {
                    tracing::error!("NM read error: {e}");
                    break;
                }
            }
        }
    })
}

fn process_nm_message(
    mut msg: serde_json::Value,
    pending: &Arc<Mutex<Pending>>,
    nm_tx: &mpsc::Sender<serde_json::Value>,
    version_gate: &VersionGate,
) -> Result<(), &'static str> {
    // Ping/Pong are intra-host keepalives; never reach pending.
    let is_pong = msg.pointer("/result/type").and_then(|v| v.as_str()) == Some("Pong");
    let is_ping = msg.pointer("/command/type").and_then(|v| v.as_str()) == Some("Ping");

    if is_pong {
        return Ok(());
    }
    if is_ping {
        // The extension stamps every keepalive (and the connect-time hello)
        // with its manifest version; resolve the gate against the bundled
        // version. A Ping with *no* version is a pre-handshake extension —
        // treat that as a mismatch, not as "unknown/allow", so a stale install
        // that predates this protocol is still caught.
        let reported = msg
            .pointer("/command/extension_version")
            .and_then(|v| v.as_str());
        let state = match reported {
            Some(v) if v == assets::expected_extension_version() => GateState::Matched,
            Some(v) => GateState::Mismatch(v.to_owned()),
            None => GateState::Mismatch(String::new()),
        };
        if let Ok(mut gate) = version_gate.lock() {
            *gate = state;
        }
        let id = msg.get("id").and_then(|v| v.as_u64()).ok_or("missing id")?;
        let pong = serde_json::json!({"id": id, "result": {"type": "Pong"}});
        let _ = nm_tx.blocking_send(pong);
        // Push the host's resolved settings alongside every Pong. Every Ping —
        // the connect-time hello AND each keepalive — re-delivers it, because
        // an MV3 worker is routinely suspended and restarted with empty state;
        // re-Pinging on wake is exactly when it needs the config again. The
        // payload carries only values whose defaults already agree across
        // modes (today: the navigation timeout), so applying it never changes
        // untuned behaviour; the schema is versioned and unknown fields are
        // ignored so future tunables can ride the same channel.
        let config = serde_json::json!({
            "result": {
                "type": "Config",
                "schema": 1,
                "timeouts": {
                    "navigation_ms":
                        webpilot::settings::timeouts().navigation.as_millis() as u64,
                    "annotation_paint_ms":
                        webpilot::settings::timeouts().annotation_paint.as_millis() as u64,
                },
            },
        });
        let _ = nm_tx.blocking_send(config);
        return Ok(());
    }

    // Persist screenshot bodies to artifact dir, replace b64 field with path.
    if let Some(b64) = msg
        .pointer("/result/screenshot_b64")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    {
        match webpilot::screenshot::process_and_save(&b64, &dirs::artifact_path("capture", "png")) {
            Ok(info) => {
                tracing::info!(
                    "Screenshot: {} ({}x{}, {}KB, ~{} tokens)",
                    info.path.display(),
                    info.width,
                    info.height,
                    info.bytes / 1024,
                    info.estimated_tokens
                );
                if let Some(result) = msg.get_mut("result")
                    && let Some(obj) = result.as_object_mut()
                {
                    obj.insert(
                        "screenshot_path".into(),
                        serde_json::json!(info.path.to_string_lossy()),
                    );
                    // The saved dimensions — and the downscale ratio when one
                    // was applied — ride along (headless parity): pixel
                    // coordinates on the saved image map to page pixels via
                    // `coord / scale`, and a silent resize breaks that math.
                    obj.insert("screenshot_width".into(), serde_json::json!(info.width));
                    obj.insert("screenshot_height".into(), serde_json::json!(info.height));
                    if info.scale != 1.0 {
                        obj.insert("screenshot_scale".into(), serde_json::json!(info.scale));
                    }
                    obj.remove("screenshot_b64");
                }
            }
            Err(e) => {
                tracing::error!("Screenshot save failed: {e}");
                // Surface the failure on the response instead of letting the
                // capture deserialize as a silent success with no image.
                if let Some(result) = msg.get_mut("result")
                    && let Some(obj) = result.as_object_mut()
                {
                    obj.insert("screenshot_error".into(), serde_json::json!(e.to_string()));
                    obj.remove("screenshot_b64");
                }
            }
        }
    }

    // Persist session_data to artifact dir.
    if let Some(data) = msg
        .pointer("/result/session_data")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    {
        let path = dirs::artifact_path("session", "json");
        match dirs::atomic_write(&path, data.as_bytes()) {
            Ok(_) => {
                if let Some(result) = msg.get_mut("result")
                    && let Some(obj) = result.as_object_mut()
                {
                    obj.insert("path".into(), serde_json::json!(path.to_string_lossy()));
                    obj.remove("session_data");
                }
            }
            Err(e) => {
                // An export the host could not persist is a failure, not a
                // success with a missing file — surface it typed.
                msg["result"] = serde_json::json!({
                    "type": "Error",
                    "error": {
                        "code": "Other",
                        "message": format!("Session save failed at {}: {e}", path.display()),
                    },
                });
            }
        }
    }

    // Dispatch by request id. Request ids are a `u32` counter, so a response id
    // that doesn't fit `u32` can't name a real pending slot — reject it rather
    // than truncate, which could resolve to the wrong slot (e.g. 2^32+1 → 1).
    let id = msg
        .get("id")
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
        .ok_or("response missing or out-of-range id")?;

    let mut pending_guard = pending.blocking_lock();
    if let Some(sender) = pending_guard.remove(&id) {
        let _ = sender.send(msg);
    } else {
        tracing::debug!(id, "received response for no pending request");
    }
    Ok(())
}

async fn handle_ipc_connections(
    listener: UnixListener,
    nm_tx: mpsc::Sender<serde_json::Value>,
    pending: Arc<Mutex<Pending>>,
    ids: Arc<AtomicU32>,
    version_gate: VersionGate,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let nm_tx = nm_tx.clone();
                let pending = pending.clone();
                let ids = ids.clone();
                let version_gate = version_gate.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        handle_one_cli_request(stream, nm_tx, pending, ids, version_gate).await
                    {
                        tracing::debug!("IPC request failed: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::error!("IPC accept error: {e}");
                break;
            }
        }
    }
}

async fn handle_one_cli_request(
    stream: tokio::net::UnixStream,
    nm_tx: mpsc::Sender<serde_json::Value>,
    pending: Arc<Mutex<Pending>>,
    ids: Arc<AtomicU32>,
    version_gate: VersionGate,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let reader = BufReader::new(reader);

    // Bound the request read in BOTH size and time. The socket is 0600 (same
    // user), so this is hygiene rather than a security boundary, but an
    // unbounded `read_line` lets a client that never sends a newline — or
    // streams without one — grow a single line without limit, and a client
    // that connects and sends nothing pins the spawned task forever. `take`
    // caps the bytes; the timeout caps the wait.
    const MAX_REQUEST_BYTES: u64 = 8 * 1024 * 1024;
    const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    let mut line = String::new();
    let n = match tokio::time::timeout(
        READ_TIMEOUT,
        reader.take(MAX_REQUEST_BYTES + 1).read_line(&mut line),
    )
    .await
    {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => anyhow::bail!("IPC request read timed out"),
    };
    if n == 0 {
        return Ok(()); // client closed without sending a request
    }
    if line.len() as u64 > MAX_REQUEST_BYTES {
        return reply_error(
            &mut writer,
            None,
            WebPilotError::InvalidArgument {
                detail: format!("request exceeds the {MAX_REQUEST_BYTES}-byte limit"),
            },
        )
        .await;
    }
    let request: serde_json::Value = serde_json::from_str(line.trim())?;

    let cli_id = request.get("id").cloned();

    // Gate every command on the version handshake. The extension Pings on
    // connect, but a CLI request can race ahead of that first Ping; wait a
    // brief bounded window for the gate to resolve rather than forwarding an
    // unverified command. A skewed install is rejected loudly (every command,
    // not just `status`); a gate still unresolved after the grace fails
    // closed — a command must not reach an extension whose protocol version
    // was never confirmed.
    match resolve_gate(&version_gate).await {
        GateState::Matched => {}
        GateState::Mismatch(extension) => {
            let reported = if extension.is_empty() {
                "unknown".to_owned()
            } else {
                extension
            };
            return reply_error(
                &mut writer,
                cli_id,
                WebPilotError::VersionMismatch {
                    extension: reported,
                    expected: assets::expected_extension_version().to_owned(),
                },
            )
            .await;
        }
        GateState::Unknown => {
            return reply_error(
                &mut writer,
                cli_id,
                WebPilotError::ConnectionLost {
                    detail: "extension has not completed its version handshake".into(),
                },
            )
            .await;
        }
    }

    // Enforce policy *here*, in the process that actually reaches the
    // authenticated browser — the CLI-side `IpcTransport` is only a socket
    // writer and could be bypassed by writing the socket directly.
    //
    // `parse_and_enforce` both validates the payload as a typed `Command`
    // (rejecting anything the strict Rust types refuse but the loose JS bridge
    // would coerce — e.g. a string index) and applies the deny rules, so only
    // a validated, permitted command is forwarded below.
    let command = match crate::policy::parse_and_enforce(&request["command"]) {
        Ok(command) => command,
        Err(e) => return reply_error(&mut writer, cli_id, e).await,
    };
    // Forward a FRESHLY BUILT envelope — never the caller's JSON mutated in
    // place. Re-serializing `command` strips unmodeled fields INSIDE it, but a
    // direct socket writer could also append a top-level SIBLING the extension
    // acts on — notably `result: { type: "Config" }`, which the service worker
    // applies and then EARLY-RETURNS on (`host.js`), dropping this command (the
    // CLI then hangs out its whole response timeout) while adopting an
    // attacker-chosen config. Emitting only the three protocol-defined fields
    // discards every such field by construction, completing the "forward only
    // what policy validated" intent the command re-serialization began.
    let host_id = ids.fetch_add(1, Ordering::Relaxed);
    let request = serde_json::json!({
        // A host-unique id (not the CLI's, which only correlates one socket) so it
        // can't clash with another concurrent CLI process over the multiplexed NM
        // channel; `cli_id` is restored on the response's way back.
        "id": host_id,
        // The re-serialized parsed command — the validated, permitted shape, with
        // any field the strict `Command` types don't model already stripped.
        "command": serde_json::to_value(&command).expect("Command serializes (static shape)"),
        // The service worker re-arms MAIN-world console/network hooks after every
        // navigation but never reads the policy store (the host is the sole sink);
        // carry the current `eval`-gate verdicts so its re-arm honours a deny that
        // landed after the monitor started — keeping the enforce boundary identical
        // to headless `reinstall_monitors`, which re-checks the very same gate.
        "monitor_policy": {
            "console": crate::policy::enforce(&webpilot::protocol::Command::ConsoleStart).is_ok(),
            "network": crate::policy::enforce(&webpilot::protocol::Command::NetworkStart).is_ok(),
        },
    });

    // Reject a command too large for the Native Messaging frame BEFORE
    // enqueueing it: the writer would otherwise drop the oversized message and
    // leave the CLI waiting out the full response timeout for a reply that can
    // never come. A typed error now is the honest, immediate answer.
    if let Ok(encoded) = serde_json::to_vec(&request)
        && encoded.len() > native_messaging::MAX_WRITE_SIZE
    {
        return reply_error(
            &mut writer,
            cli_id,
            WebPilotError::InvalidArgument {
                detail: format!(
                    "command is {} bytes, over the {}-byte Native Messaging frame limit — retry without --browser (headless reads the file directly)",
                    encoded.len(),
                    native_messaging::MAX_WRITE_SIZE
                ),
            },
        )
        .await;
    }

    let (resp_tx, resp_rx) = oneshot::channel();
    pending.lock().await.insert(host_id, resp_tx);

    // If the message can't be handed to the writer, drop the pending slot we
    // just reserved — otherwise a failed send leaks an entry forever.
    if let Err(e) = nm_tx.send(request).await {
        pending.lock().await.remove(&host_id);
        return Err(e.into());
    }

    let timeout = webpilot::settings::timeouts().ipc_response;
    let mut response = match tokio::time::timeout(timeout, resp_rx).await {
        Ok(Ok(v)) => v,
        Ok(Err(_)) => {
            pending.lock().await.remove(&host_id);
            anyhow::bail!("response channel closed");
        }
        Err(_) => {
            pending.lock().await.remove(&host_id);
            anyhow::bail!("response timeout after {}s", timeout.as_secs());
        }
    };

    if let Some(id) = cli_id {
        response["id"] = id;
    }

    write_response(&mut writer, &response).await
}

/// Frame and write a JSON-RPC response, BOUNDED by the same timeout the request
/// wait uses: a peer that stopped reading (a CLI killed mid-read) must not park
/// this task on the socket forever, holding its channel sender and accreting
/// per-connection tasks while Chrome is still alive. On expiry the write fails,
/// the connection closes, and the task ends.
async fn write_response<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    response: &serde_json::Value,
) -> anyhow::Result<()> {
    let mut payload = serde_json::to_vec(response)?;
    payload.push(b'\n');
    let timeout = webpilot::settings::timeouts().ipc_response;
    match tokio::time::timeout(timeout, writer.write_all(&payload)).await {
        Ok(r) => Ok(r?),
        Err(_) => anyhow::bail!("response write timed out after {}s", timeout.as_secs()),
    }
}

fn current_gate(gate: &VersionGate) -> GateState {
    gate.lock().map(|g| g.clone()).unwrap_or(GateState::Unknown)
}

/// Resolve the version gate, waiting a brief bounded window for the connect-time
/// Ping to land if it has not yet (a CLI request can race ahead of it). Returns
/// the first non-`Unknown` state, or `Unknown` if the handshake never arrives.
async fn resolve_gate(gate: &VersionGate) -> GateState {
    const POLL: std::time::Duration = std::time::Duration::from_millis(20);
    let deadline = tokio::time::Instant::now() + webpilot::settings::timeouts().version_handshake;
    loop {
        let state = current_gate(gate);
        if !matches!(state, GateState::Unknown) || tokio::time::Instant::now() >= deadline {
            return state;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Write a typed error back to the CLI as a `ResponseData::Error` envelope —
/// the same shape a command failure takes, so the client surfaces it through
/// the normal error path (exit code from `WebPilotError`).
async fn reply_error<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    cli_id: Option<serde_json::Value>,
    error: WebPilotError,
) -> anyhow::Result<()> {
    let mut response = serde_json::json!({
        "result": { "type": "Error", "error": error.to_wire() },
    });
    if let Some(id) = cli_id {
        response["id"] = id;
    }
    write_response(writer, &response).await
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_log_rotates_instead_of_being_truncated() {
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!("wp-hostlog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("host.log");
        let rotated = dir.join("host.log.1");

        // Under the bound the log is appended to, so one session can read what
        // the one before it recorded.
        open_log_at(&path).unwrap().write_all(b"first\n").unwrap();
        open_log_at(&path).unwrap().write_all(b"second\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");
        assert!(!rotated.exists());

        // At the bound the full log is renamed, never truncated: the previous
        // session's account is what a report about it needs.
        std::fs::write(&path, vec![b'x'; LOG_ROTATE_BYTES as usize]).unwrap();
        open_log_at(&path).unwrap().write_all(b"after\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after\n");
        assert_eq!(
            std::fs::metadata(&rotated).unwrap().len(),
            LOG_ROTATE_BYTES,
            "the full log must survive as the rotated one"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
    use super::*;

    /// A Ping must resolve the version gate, answer with a Pong, AND push the
    /// host's resolved settings — the Config ride-along is what lets a
    /// restarted (state-wiped) worker re-learn tuned timeouts on its next
    /// keepalive, so browser mode honors the same `webpilot::settings` the
    /// headless transport reads directly.
    #[test]
    fn ping_resolves_gate_and_pushes_pong_then_config() {
        let (tx, mut rx) = mpsc::channel(4);
        let pending: Arc<Mutex<Pending>> = Arc::new(Mutex::new(Pending::new()));
        let gate: VersionGate = Arc::new(std::sync::Mutex::new(GateState::Unknown));

        let ping = serde_json::json!({
            "id": 7,
            "command": {
                "type": "Ping",
                "extension_version": assets::expected_extension_version(),
            },
        });
        process_nm_message(ping, &pending, &tx, &gate).expect("ping processed");

        let pong = rx.try_recv().expect("a Pong reply");
        assert_eq!(
            pong.pointer("/result/type").and_then(|v| v.as_str()),
            Some("Pong")
        );

        let config = rx.try_recv().expect("a Config push after the Pong");
        assert_eq!(
            config.pointer("/result/type").and_then(|v| v.as_str()),
            Some("Config")
        );
        let nav = config
            .pointer("/result/timeouts/navigation_ms")
            .and_then(|v| v.as_u64())
            .expect("navigation_ms present");
        assert_eq!(
            nav,
            webpilot::settings::timeouts().navigation.as_millis() as u64,
            "the pushed value must be the host's RESOLVED setting"
        );

        assert!(
            matches!(current_gate(&gate), GateState::Matched),
            "a matching version Ping must resolve the gate"
        );
    }
}
