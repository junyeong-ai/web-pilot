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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{Mutex, mpsc, oneshot};

use webpilot::{dirs, ipc, native_messaging};

type Pending = std::collections::HashMap<u32, oneshot::Sender<serde_json::Value>>;

pub async fn run_host() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();

    tracing::info!("WebPilot host starting");

    let pending: Arc<Mutex<Pending>> = Arc::new(Mutex::new(Pending::new()));
    let (nm_tx, nm_rx) = mpsc::channel::<serde_json::Value>(32);

    let ipc_listener = ipc::start_server().await?;

    let nm_writer_handle = spawn_nm_writer(nm_rx);
    let nm_reader_handle = spawn_nm_reader(pending.clone(), nm_tx.clone());
    // The host owns the id space facing Chrome: each forwarded request gets a
    // host-unique id, so concurrent CLI processes (which each start their own
    // counter) can never collide in `pending`.
    let ids = Arc::new(AtomicU32::new(1));
    let ipc_handle = tokio::spawn(handle_ipc_connections(
        ipc_listener,
        nm_tx.clone(),
        pending.clone(),
        ids,
    ));

    tracing::info!("Host ready");

    // The reader exits when Chrome disconnects — that's our shutdown signal.
    let _ = nm_reader_handle.await;
    let _ = std::fs::remove_file(ipc::socket_path());

    drop(nm_tx);
    let _ = nm_writer_handle.await;
    ipc_handle.abort();

    Ok(())
}

fn spawn_nm_writer(mut rx: mpsc::Receiver<serde_json::Value>) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        while let Some(msg) = rx.blocking_recv() {
            let mut stdout = std::io::stdout().lock();
            if let Err(e) = native_messaging::write_message(&mut stdout, &msg) {
                tracing::error!("NM write error: {e}");
                break;
            }
        }
    })
}

fn spawn_nm_reader(
    pending: Arc<Mutex<Pending>>,
    nm_tx: mpsc::Sender<serde_json::Value>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut stdin = std::io::stdin().lock();
        loop {
            match native_messaging::read_message(&mut stdin) {
                Ok(msg) => {
                    if let Err(e) = process_nm_message(msg, &pending, &nm_tx) {
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
) -> Result<(), &'static str> {
    // Ping/Pong are intra-host keepalives; never reach pending.
    let is_pong = msg.pointer("/result/type").and_then(|v| v.as_str()) == Some("Pong");
    let is_ping = msg.pointer("/command/type").and_then(|v| v.as_str()) == Some("Ping");

    if is_pong {
        return Ok(());
    }
    if is_ping {
        let id = msg.get("id").and_then(|v| v.as_u64()).ok_or("missing id")?;
        let pong = serde_json::json!({"id": id, "result": {"type": "Pong"}});
        let _ = nm_tx.blocking_send(pong);
        return Ok(());
    }

    // Persist screenshot bodies to artifact dir, replace b64 field with path.
    if let Some(b64) = msg
        .pointer("/result/screenshot_b64")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    {
        let dir = dirs::artifacts_dir();
        match webpilot::screenshot::process_and_save(&b64, &dir) {
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
                    obj.remove("screenshot_b64");
                }
            }
            Err(e) => tracing::error!("Screenshot save failed: {e}"),
        }
    }

    // Persist session_data to artifact dir.
    if let Some(data) = msg
        .pointer("/result/session_data")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    {
        let dir = dirs::artifacts_dir();
        let ts = epoch_ms();
        let path = dir.join(format!("session_{ts}.json"));
        match std::fs::write(&path, &data) {
            Ok(_) => {
                if let Some(result) = msg.get_mut("result")
                    && let Some(obj) = result.as_object_mut()
                {
                    obj.insert("path".into(), serde_json::json!(path.to_string_lossy()));
                    obj.remove("session_data");
                }
            }
            Err(e) => tracing::error!("Session save failed: {e}"),
        }
    }

    // Dispatch by request id.
    let id = msg
        .get("id")
        .and_then(|v| v.as_u64())
        .ok_or("response missing id")? as u32;

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
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let nm_tx = nm_tx.clone();
                let pending = pending.clone();
                let ids = ids.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_one_cli_request(stream, nm_tx, pending, ids).await {
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
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let mut request: serde_json::Value = serde_json::from_str(line.trim())?;

    // The CLI's own id only correlates this one socket's request/response, so we
    // restore it on the way back; over the multiplexed NM channel we use a
    // host-unique id that can't clash with another concurrent CLI process.
    let cli_id = request.get("id").cloned();
    let host_id = ids.fetch_add(1, Ordering::Relaxed);
    request["id"] = host_id.into();

    let (resp_tx, resp_rx) = oneshot::channel();
    pending.lock().await.insert(host_id, resp_tx);

    nm_tx.send(request).await?;

    let timeout = crate::timeouts::ipc_response();
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

    let mut payload = serde_json::to_vec(&response)?;
    payload.push(b'\n');
    writer.write_all(&payload).await?;

    Ok(())
}

fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
