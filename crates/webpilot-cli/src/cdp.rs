//! CDP (Chrome DevTools Protocol) client via WebSocket.
//! Used for headless mode — communicates directly with Chrome, no Extension needed.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct CdpClient {
    writer: Arc<Mutex<futures_util::stream::SplitSink<WsStream, Message>>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: Arc<Mutex<u64>>,
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

        // Background reader: dispatch responses to pending requests
        let pending_clone = pending.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = reader.next().await {
                if let Message::Text(text) = msg
                    && let Ok(json) = serde_json::from_str::<Value>(text.as_ref())
                    && let Some(id) = json.get("id").and_then(|v| v.as_u64())
                {
                    let mut map = pending_clone.lock().await;
                    if let Some(sender) = map.remove(&id) {
                        let _ = sender.send(json);
                    }
                }
            }
        });

        Ok(Self {
            writer,
            pending,
            next_id,
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
        self.send("Page.enable", None).await?;
        self.send("Page.navigate", Some(serde_json::json!({"url": url})))
            .await?;
        // Wait for loadEventFired would require event subscription; use sleep as pragmatic approach
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        Ok(())
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
}
