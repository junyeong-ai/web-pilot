//! CDP (Chrome DevTools Protocol) client via WebSocket.
//! Used for headless mode — communicates directly with Chrome, no Extension needed.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct CdpClient {
    writer: Arc<Mutex<futures_util::stream::SplitSink<WsStream, Message>>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: Arc<AtomicU64>,
    events: broadcast::Sender<Value>,
    alive: Arc<AtomicBool>,
    reader_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    heartbeat_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl CdpClient {
    /// Connect to Chrome's CDP WebSocket endpoint.
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let (ws, _) = connect_async(ws_url)
            .await
            .context("Failed to connect to Chrome CDP")?;

        let (writer, mut reader) = ws.split();
        let writer = Arc::new(Mutex::new(writer));
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let next_id = Arc::new(AtomicU64::new(1));
        let alive = Arc::new(AtomicBool::new(true));

        let buffer_size: usize = std::env::var("WEBPILOT_CDP_EVENT_BUFFER")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(256);
        let (events_tx, _) = broadcast::channel::<Value>(buffer_size);

        // Background reader: dispatch responses or broadcast events
        let pending_clone = pending.clone();
        let events_tx_clone = events_tx.clone();
        let alive_clone = alive.clone();
        let reader_handle = tokio::spawn(async move {
            while let Some(msg_result) = reader.next().await {
                match msg_result {
                    Ok(Message::Text(text)) => match serde_json::from_str::<Value>(text.as_ref()) {
                        Ok(json) => {
                            if let Some(id) = json.get("id").and_then(|v| v.as_u64()) {
                                let mut map = pending_clone.lock().await;
                                if let Some(sender) = map.remove(&id) {
                                    let _ = sender.send(json);
                                }
                            } else {
                                let _ = events_tx_clone.send(json);
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "CDP: malformed JSON: {e} (first 200 chars: {})",
                                &text[..text.len().min(200)]
                            );
                        }
                    },
                    Ok(Message::Close(frame)) => {
                        tracing::debug!("CDP WebSocket closed: {frame:?}");
                        break;
                    }
                    Ok(_) => {} // Ping/Pong/Binary — handled by tungstenite
                    Err(e) => {
                        tracing::debug!("CDP WebSocket read error: {e}");
                        break;
                    }
                }
            }
            // Reader exiting — mark connection as dead and drain all pending
            alive_clone.store(false, Ordering::Release);
            let mut map = pending_clone.lock().await;
            map.drain(); // Drop all senders → callers get RecvError
        });

        let reader_handle = Arc::new(Mutex::new(Some(reader_handle)));

        // Heartbeat: periodic health check to detect TCP half-open
        let heartbeat_handle = {
            let writer = writer.clone();
            let pending = pending.clone();
            let next_id = next_id.clone();
            let alive = alive.clone();
            let interval = crate::timeouts::heartbeat();
            Arc::new(Mutex::new(Some(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(interval).await;
                    if !alive.load(Ordering::Acquire) {
                        break;
                    }

                    let id = next_id.fetch_add(1, Ordering::Relaxed);
                    let msg = serde_json::json!({
                        "id": id,
                        "method": "Browser.getVersion",
                        "params": {},
                    });

                    let (tx, rx) = oneshot::channel();
                    pending.lock().await.insert(id, tx);

                    let send_result = writer
                        .lock()
                        .await
                        .send(Message::Text(
                            serde_json::to_string(&msg).unwrap_or_default().into(),
                        ))
                        .await;

                    if send_result.is_err() {
                        alive.store(false, Ordering::Release);
                        pending.lock().await.remove(&id);
                        break;
                    }

                    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
                        Ok(Ok(_)) => {} // Healthy
                        _ => {
                            tracing::warn!("CDP heartbeat failed — marking connection dead");
                            alive.store(false, Ordering::Release);
                            pending.lock().await.remove(&id);
                            break;
                        }
                    }
                }
            }))))
        };

        Ok(Self {
            writer,
            pending,
            next_id,
            events: events_tx,
            alive,
            reader_handle,
            heartbeat_handle,
        })
    }

    /// Check if the CDP connection is still alive.
    #[allow(dead_code)]
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    /// Send a CDP command and wait for response (default timeout).
    pub async fn send(&self, method: &str, params: Option<Value>) -> Result<Value> {
        self.send_with_timeout(method, params, crate::timeouts::cdp_send())
            .await
    }

    /// Send a CDP command with a custom timeout.
    pub async fn send_with_timeout(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: std::time::Duration,
    ) -> Result<Value> {
        if !self.alive.load(Ordering::Acquire) {
            anyhow::bail!("CDP connection is dead (reader exited)");
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let msg = serde_json::json!({
            "id": id,
            "method": method,
            "params": params.unwrap_or(Value::Object(Default::default())),
        });

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        self.writer
            .lock()
            .await
            .send(Message::Text(serde_json::to_string(&msg)?.into()))
            .await
            .context("CDP send failed")?;

        let response = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => {
                self.pending.lock().await.remove(&id);
                anyhow::bail!("CDP channel closed");
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                anyhow::bail!("CDP timeout ({}s)", timeout.as_secs());
            }
        };

        if let Some(error) = response.get("error") {
            anyhow::bail!("CDP error: {}", error);
        }

        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Fire a CDP command without waiting for response.
    /// Used for Page.navigate where cross-origin navigation may kill the connection
    /// before Chrome sends a response.
    #[allow(dead_code)]
    pub async fn fire(&self, method: &str, params: Option<Value>) -> Result<()> {
        if !self.alive.load(Ordering::Acquire) {
            anyhow::bail!("CDP connection is dead (reader exited)");
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let msg = serde_json::json!({
            "id": id,
            "method": method,
            "params": params.unwrap_or(Value::Object(Default::default())),
        });

        // Don't register in pending — we don't care about the response
        self.writer
            .lock()
            .await
            .send(Message::Text(serde_json::to_string(&msg)?.into()))
            .await
            .context("CDP fire failed")?;

        Ok(())
    }

    /// Evaluate JavaScript in the page context.
    pub async fn evaluate(&self, expression: &str) -> Result<Value> {
        let result = self
            .send(
                "Runtime.evaluate",
                Some(serde_json::json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                })),
            )
            .await?;

        if let Some(exception) = result.get("exceptionDetails") {
            let msg = exception
                .pointer("/exception/description")
                .or(exception.pointer("/text"))
                .and_then(|v| v.as_str())
                .unwrap_or("JS exception");
            anyhow::bail!("{msg}");
        }

        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Navigate to URL and wait for load.
    pub async fn navigate(&self, url: &str) -> Result<()> {
        let resp = self
            .send("Page.navigate", Some(serde_json::json!({"url": url})))
            .await?;
        if let Some(err) = resp.get("errorText").and_then(|v| v.as_str()) {
            anyhow::bail!("Navigation failed: {err}");
        }
        match self
            .wait_for_event("Page.loadEventFired", crate::timeouts::navigation())
            .await
        {
            Ok(_) => Ok(()),
            Err(_) => {
                tokio::time::sleep(crate::timeouts::post_reconnect()).await;
                Ok(())
            }
        }
    }

    /// Wait for a CDP event by method name, with timeout.
    pub async fn wait_for_event(
        &self,
        method: &str,
        timeout: std::time::Duration,
    ) -> Result<Value> {
        let mut rx = self.events.subscribe();
        let method = method.to_string();
        let method_for_err = method.clone();
        match tokio::time::timeout(timeout, async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if event.get("method").and_then(|v| v.as_str()) == Some(&method) {
                            return Ok(event);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        anyhow::bail!("CDP event channel closed");
                    }
                }
            }
        })
        .await
        {
            Ok(result) => result,
            Err(_) => anyhow::bail!("Timeout waiting for {method_for_err}"),
        }
    }

    /// Capture screenshot as base64 PNG.
    pub async fn screenshot(&self) -> Result<String> {
        let result = self
            .send(
                "Page.captureScreenshot",
                Some(serde_json::json!({
                    "format": "png",
                })),
            )
            .await?;
        result
            .get("data")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("No screenshot data"))
    }

    /// Get all cookies.
    pub async fn get_cookies(&self) -> Result<Vec<Value>> {
        let result = self.send("Network.getCookies", None).await?;
        Ok(result
            .get("cookies")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Get browser targets (tabs).
    pub async fn get_targets(&self) -> Result<Vec<Value>> {
        let result = self.send("Target.getTargets", None).await?;
        Ok(result
            .get("targetInfos")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Get all browser context IDs.
    pub async fn get_browser_contexts(&self) -> Result<Vec<String>> {
        let result = self.send("Target.getBrowserContexts", None).await?;
        Ok(result
            .get("browserContextIds")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Create an isolated browser context (separate cookies, cache, storage).
    pub async fn create_browser_context(&self) -> Result<String> {
        let result = self
            .send(
                "Target.createBrowserContext",
                Some(serde_json::json!({"disposeOnDetach": false})),
            )
            .await?;
        result
            .get("browserContextId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("No browserContextId in response"))
    }

    /// Dispose (destroy) a browser context and all its targets.
    pub async fn dispose_browser_context(&self, browser_context_id: &str) -> Result<()> {
        self.send(
            "Target.disposeBrowserContext",
            Some(serde_json::json!({"browserContextId": browser_context_id})),
        )
        .await?;
        Ok(())
    }

    /// Create a new page target within a specific browser context.
    pub async fn create_target_in_context(
        &self,
        browser_context_id: &str,
        url: &str,
    ) -> Result<String> {
        let result = self
            .send(
                "Target.createTarget",
                Some(serde_json::json!({
                    "url": url,
                    "browserContextId": browser_context_id,
                })),
            )
            .await?;
        result
            .get("targetId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("No targetId in response"))
    }
}

impl Drop for CdpClient {
    fn drop(&mut self) {
        // Abort background tasks to prevent resource leaks
        if let Ok(mut handle) = self.reader_handle.try_lock()
            && let Some(h) = handle.take()
        {
            h.abort();
        }
        if let Ok(mut handle) = self.heartbeat_handle.try_lock()
            && let Some(h) = handle.take()
        {
            h.abort();
        }
    }
}
