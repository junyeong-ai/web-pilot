//! Native Messaging Host mode.
//!
//! Launched by Chrome when the Extension calls connectNative().
//! Bridges CLI commands (via Unix Socket) to the Extension (via NM stdin/stdout).

use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{Mutex, mpsc};

use webpilot::ipc;
use webpilot::native_messaging;

struct HostState {
    pending: std::collections::HashMap<
        u32,
        (
            tokio::sync::oneshot::Sender<serde_json::Value>,
            tokio::time::Instant,
        ),
    >,
}

pub async fn run_host() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();

    tracing::info!("WebPilot host starting");

    let state = Arc::new(Mutex::new(HostState {
        pending: std::collections::HashMap::new(),
    }));

    let (nm_tx, nm_rx) = mpsc::channel::<serde_json::Value>(32);

    let ipc_listener = match ipc::start_server().await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to start IPC server: {e}");
            return Err(e.into());
        }
    };

    // NM stdout writer — acquire/release lock per message to avoid Chrome pipe issues
    let nm_writer_handle = tokio::task::spawn_blocking({
        let mut nm_rx = nm_rx;
        move || {
            while let Some(msg) = nm_rx.blocking_recv() {
                let mut stdout = std::io::stdout().lock();
                if let Err(e) = native_messaging::write_message(&mut stdout, &msg) {
                    tracing::error!("NM write error: {e}");
                    break;
                }
            }
        }
    });

    // NM stdin reader
    let state_reader = state.clone();
    let nm_tx_reader = nm_tx.clone();
    let nm_reader_handle = tokio::task::spawn_blocking(move || {
        let mut stdin = std::io::stdin().lock();
        loop {
            match native_messaging::read_message(&mut stdin) {
                Ok(mut msg) => {
                    // Handle Ping → respond with Pong (echo the request ID)
                    let is_pong =
                        msg.pointer("/result/type").and_then(|v| v.as_str()) == Some("Pong");
                    let is_ping =
                        msg.pointer("/command/type").and_then(|v| v.as_str()) == Some("Ping");

                    if is_pong {
                        continue;
                    }
                    if is_ping {
                        let ping_id = msg.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                        let pong = serde_json::json!({"id": ping_id, "result": {"type": "Pong"}});
                        let _ = nm_tx_reader.blocking_send(pong);
                        continue;
                    }

                    // Process screenshot: decode base64, resize, save to file
                    if let Some(b64) = msg
                        .pointer("/result/screenshot_b64")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                    {
                        let output_dir = std::path::Path::new(webpilot::OUTPUT_DIR);
                        match webpilot::screenshot::process_and_save(&b64, output_dir) {
                            Ok(info) => {
                                tracing::info!(
                                    "Screenshot: {} ({}x{}, {}KB, ~{} tokens)",
                                    info.path.display(),
                                    info.width,
                                    info.height,
                                    info.bytes / 1024,
                                    info.estimated_tokens
                                );
                                if let Some(result) = msg.get_mut("result") {
                                    result["screenshot_path"] =
                                        serde_json::json!(info.path.to_string_lossy());
                                    result.as_object_mut().map(|o| o.remove("screenshot_b64"));
                                }
                            }
                            Err(e) => tracing::error!("Screenshot save error: {e}"),
                        }
                    }

                    // Session export: save session_data to file
                    if let Some(data) = msg
                        .pointer("/result/session_data")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                    {
                        let output_dir = std::path::Path::new(webpilot::OUTPUT_DIR);
                        let _ = std::fs::create_dir_all(output_dir);
                        let ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis();
                        let path = output_dir.join(format!("session_{ts}.json"));
                        if let Err(e) = std::fs::write(&path, &data) {
                            tracing::error!("Session save error: {e}");
                        } else if let Some(result) = msg.get_mut("result") {
                            result["path"] = serde_json::json!(path.to_string_lossy());
                            result.as_object_mut().map(|o| o.remove("session_data"));
                        }
                    }

                    // Dispatch to pending CLI request
                    if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
                        let mut st = state_reader.blocking_lock();
                        if let Some((sender, _)) = st.pending.remove(&(id as u32)) {
                            let _ = sender.send(msg);
                        }
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
    });

    // Orphan reaper: clean up stale pending entries every 30s
    let state_reaper = state.clone();
    let reaper_handle = tokio::spawn(async move {
        let max_age = std::time::Duration::from_secs(120);
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let mut st = state_reaper.lock().await;
            let now = tokio::time::Instant::now();
            st.pending
                .retain(|_id, (_sender, created)| now.duration_since(*created) < max_age);
        }
    });

    // IPC handler
    let ipc_handle = tokio::spawn(handle_ipc_connections(
        ipc_listener,
        nm_tx.clone(),
        state.clone(),
    ));

    tracing::info!("Host ready");

    let _ = nm_reader_handle.await;
    let path = ipc::socket_path();
    let _ = std::fs::remove_file(&path);

    drop(nm_tx);
    let _ = nm_writer_handle.await;
    ipc_handle.abort();
    reaper_handle.abort();

    Ok(())
}

async fn handle_ipc_connections(
    listener: UnixListener,
    nm_tx: mpsc::Sender<serde_json::Value>,
    state: Arc<Mutex<HostState>>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let nm_tx = nm_tx.clone();
                let state = state.clone();
                tokio::spawn(async move {
                    let _ = handle_one_cli_request(stream, nm_tx, state).await;
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
    state: Arc<Mutex<HostState>>,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let request: serde_json::Value = serde_json::from_str(line.trim())?;

    let id = request.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

    let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
    {
        let mut st = state.lock().await;
        // Duplicate ID guard: if a request with this ID is already pending, reject
        if st.pending.contains_key(&id) {
            let err_response = serde_json::json!({
                "id": id,
                "result": {"type": "Error", "message": "Duplicate request ID", "code": "Unknown"}
            });
            let mut payload = serde_json::to_vec(&err_response)?;
            payload.push(b'\n');
            writer.write_all(&payload).await?;
            return Ok(());
        }
        st.pending
            .insert(id, (resp_tx, tokio::time::Instant::now()));
    }

    nm_tx.send(request).await?;

    let timeout = crate::timeouts::ipc_response();
    let response = match tokio::time::timeout(timeout, resp_rx).await {
        Ok(Ok(v)) => v,
        Ok(Err(_)) => {
            state.lock().await.pending.remove(&id);
            anyhow::bail!("channel closed");
        }
        Err(_) => {
            state.lock().await.pending.remove(&id);
            anyhow::bail!("timeout ({}s)", timeout.as_secs());
        }
    };

    let mut payload = serde_json::to_vec(&response)?;
    payload.push(b'\n');
    writer.write_all(&payload).await?;

    Ok(())
}
