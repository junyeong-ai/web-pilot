//! CDP (Chrome DevTools Protocol) client via WebSocket.
//! Used for headless mode — communicates directly with Chrome, no Extension needed.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct CdpClient {
    writer: Arc<Mutex<futures_util::stream::SplitSink<WsStream, Message>>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: Arc<Mutex<u64>>,
    events: broadcast::Sender<Value>,
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
        let next_id = Arc::new(Mutex::new(1u64));
        let (events_tx, _) = broadcast::channel::<Value>(64);

        // Background reader: dispatch responses or broadcast events
        let pending_clone = pending.clone();
        let events_tx_clone = events_tx.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = reader.next().await {
                if let Message::Text(text) = msg
                    && let Ok(json) = serde_json::from_str::<Value>(text.as_ref())
                {
                    if let Some(id) = json.get("id").and_then(|v| v.as_u64()) {
                        // Response to a request
                        let mut map = pending_clone.lock().await;
                        if let Some(sender) = map.remove(&id) {
                            let _ = sender.send(json);
                        }
                    } else {
                        // CDP event (no id) — broadcast
                        let _ = events_tx_clone.send(json);
                    }
                }
            }
        });

        Ok(Self {
            writer,
            pending,
            next_id,
            events: events_tx,
        })
    }

    /// Send a CDP command and wait for response.
    pub async fn send(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = {
            let mut n = self.next_id.lock().await;
            let id = *n;
            *n += 1;
            id
        };

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

        let response = match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => {
                self.pending.lock().await.remove(&id);
                anyhow::bail!("CDP channel closed");
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                anyhow::bail!("CDP timeout (30s)");
            }
        };

        if let Some(error) = response.get("error") {
            anyhow::bail!("CDP error: {}", error);
        }

        Ok(response.get("result").cloned().unwrap_or(Value::Null))
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
        // Check for navigation error (e.g., DNS failure)
        if let Some(err) = resp.get("errorText").and_then(|v| v.as_str()) {
            anyhow::bail!("Navigation failed: {err}");
        }
        // Wait for page load (15s timeout), fall back gracefully
        match self
            .wait_for_event("Page.loadEventFired", std::time::Duration::from_secs(15))
            .await
        {
            Ok(_) => Ok(()),
            Err(_) => {
                // Timeout — page may still be loading, continue anyway
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
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
