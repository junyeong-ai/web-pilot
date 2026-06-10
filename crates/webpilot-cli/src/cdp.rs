//! CDP (Chrome DevTools Protocol) client over WebSocket.
//!
//! Owns the WebSocket and routes responses back to per-request `oneshot`
//! channels. Used by `LocalTransport` to drive a headless Chrome directly,
//! without an Extension. Higher-level abstractions (page session, bridge
//! injection, browser-context lifecycle) live in `transport::local`.

use anyhow::{Context, Result, anyhow};
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
use webpilot::WebPilotError;

/// CDP reported that the execution context a request targeted no longer exists
/// — a renderer swapped the document out from under a `uniqueContextId` we still
/// held. A typed marker (not a message string) so the caller can recognise the
/// renderer-swap race and re-resolve the context, distinct from any other
/// `-32000` server error or a JS exception.
#[derive(Debug)]
pub struct ContextGone;

impl std::fmt::Display for ContextGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("execution context no longer exists")
    }
}

impl std::error::Error for ContextGone {}

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsWriter = futures_util::stream::SplitSink<WsStream, Message>;

const HEARTBEAT_TIMEOUT_S: u64 = 5;
/// Consecutive unanswered heartbeats before declaring the connection dead. A
/// single miss is tolerated because a large response can head-of-line block the
/// shared reader past one beat without the socket being dead.
const HEARTBEAT_MAX_MISSES: u32 = 3;
const PARSE_PREVIEW_CHARS: usize = 200;

pub struct CdpClient {
    writer: Arc<Mutex<WsWriter>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: Arc<AtomicU64>,
    events: broadcast::Sender<Value>,
    alive: Arc<AtomicBool>,
    reader_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    heartbeat_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl CdpClient {
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
        // Monotonic count of frames the reader has pulled off the socket. The
        // heartbeat reads it to tell a live-but-head-of-line-blocked connection
        // (traffic still arriving, our pong merely queued behind it) from a
        // genuinely silent one — so a burst of events or a large in-flight
        // response can never make a working connection look dead.
        let activity = Arc::new(AtomicU64::new(0));

        // `settings::init` rejects 0 loudly; the max(1) covers only the lazy
        // library/test path that bypasses init (broadcast panics on 0).
        let buffer_size = webpilot::settings::get().cdp.event_buffer.max(1);
        let (events_tx, _) = broadcast::channel::<Value>(buffer_size);

        // Reader: route id-bearing responses to pending channels; broadcast events.
        let pending_r = pending.clone();
        let events_r = events_tx.clone();
        let alive_r = alive.clone();
        let activity_r = activity.clone();
        let reader_handle = tokio::spawn(async move {
            while let Some(msg) = reader.next().await {
                // Any successfully-read frame proves the socket is delivering
                // data; the heartbeat samples this as its liveness signal.
                if msg.is_ok() {
                    activity_r.fetch_add(1, Ordering::Relaxed);
                }
                match msg {
                    Ok(Message::Text(text)) => match serde_json::from_str::<Value>(text.as_ref()) {
                        Ok(json) => {
                            if let Some(id) = json.get("id").and_then(|v| v.as_u64()) {
                                if let Some(sender) = pending_r.lock().await.remove(&id) {
                                    let _ = sender.send(json);
                                }
                            } else {
                                let _ = events_r.send(json);
                            }
                        }
                        Err(e) => {
                            let preview = char_safe_prefix(text.as_ref(), PARSE_PREVIEW_CHARS);
                            tracing::warn!("CDP: malformed JSON: {e} (preview: {preview})");
                        }
                    },
                    Ok(Message::Close(frame)) => {
                        tracing::debug!("CDP WebSocket closed: {frame:?}");
                        break;
                    }
                    Ok(_) => {} // Ping/Pong/Binary handled by tungstenite
                    Err(e) => {
                        tracing::debug!("CDP WebSocket read error: {e}");
                        break;
                    }
                }
            }
            // Reader exiting — mark dead and drain pending so callers get RecvError.
            alive_r.store(false, Ordering::Release);
            pending_r.lock().await.drain();
        });
        let reader_handle = Arc::new(Mutex::new(Some(reader_handle)));

        // Heartbeat: detect TCP half-open by periodically issuing Browser.getVersion.
        let heartbeat_handle = {
            let writer = writer.clone();
            let pending = pending.clone();
            let next_id = next_id.clone();
            let alive = alive.clone();
            let activity = activity.clone();
            let interval = webpilot::settings::timeouts().heartbeat;
            Arc::new(Mutex::new(Some(tokio::spawn(async move {
                let mut consecutive_misses: u32 = 0;
                loop {
                    tokio::time::sleep(interval).await;
                    if !alive.load(Ordering::Acquire) {
                        break;
                    }

                    let id = next_id.fetch_add(1, Ordering::Relaxed);
                    let msg = serde_json::json!({
                        "id": id, "method": "Browser.getVersion", "params": {},
                    });
                    let payload = match serde_json::to_string(&msg) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!("heartbeat serialize failed: {e}");
                            alive.store(false, Ordering::Release);
                            break;
                        }
                    };

                    let (tx, rx) = oneshot::channel();
                    pending.lock().await.insert(id, tx);

                    if writer
                        .lock()
                        .await
                        .send(Message::Text(payload.into()))
                        .await
                        .is_err()
                    {
                        alive.store(false, Ordering::Release);
                        pending.lock().await.drain();
                        break;
                    }

                    let timeout = std::time::Duration::from_secs(HEARTBEAT_TIMEOUT_S);
                    let activity_before = activity.load(Ordering::Relaxed);
                    match tokio::time::timeout(timeout, rx).await {
                        Ok(Ok(_)) => consecutive_misses = 0, // healthy
                        _ => {
                            // Our pong did not come back in time. Drop our own
                            // stale entry, but never drain unrelated in-flight
                            // requests on a miss. If the reader pulled ANY other
                            // frame off the socket while we waited, the
                            // connection is alive — our beat is just queued
                            // behind a backlog (a burst of events, or a large
                            // response the shared reader is still parsing), i.e.
                            // head-of-line blocking, not death. Only genuine
                            // silence — no frame at all across the wait — counts
                            // toward declaring the socket dead.
                            pending.lock().await.remove(&id);
                            if activity.load(Ordering::Relaxed) != activity_before {
                                consecutive_misses = 0;
                            } else {
                                consecutive_misses += 1;
                                if consecutive_misses >= HEARTBEAT_MAX_MISSES {
                                    tracing::warn!(
                                        "CDP heartbeat unanswered {consecutive_misses}× with no socket activity — marking connection dead"
                                    );
                                    alive.store(false, Ordering::Release);
                                    pending.lock().await.drain();
                                    break;
                                }
                            }
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

    pub async fn send(&self, method: &str, params: Option<Value>) -> Result<Value> {
        self.send_with_timeout(method, params, webpilot::settings::timeouts().cdp_send)
            .await
    }

    pub async fn send_with_timeout(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: std::time::Duration,
    ) -> Result<Value> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(WebPilotError::ConnectionLost {
                detail: format!("CDP reader exited before sending {method}"),
            }
            .into());
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = serde_json::json!({
            "id": id,
            "method": method,
            "params": params.unwrap_or(Value::Object(Default::default())),
        });

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        // Bound the WHOLE operation by ONE absolute deadline spanning both the
        // write and the response wait — not a fresh `timeout` for each phase,
        // which would let total wall time reach `2 * timeout`. The write path is
        // bounded too: acquiring the writer lock (held by a concurrent send
        // whose socket write has stalled) or the `send` itself (a full kernel
        // buffer behind a wedged peer) can otherwise hang indefinitely.
        let deadline = tokio::time::Instant::now() + timeout;
        let text = serde_json::to_string(&msg)?;
        let write = async {
            let mut guard = self.writer.lock().await;
            // Re-check liveness AFTER winning the lock: a concurrent send that
            // timed out (or the heartbeat) may have marked the connection dead
            // while we queued behind it. Dispatching now would push a frame
            // onto a sink known to be wedged.
            if !self.alive.load(Ordering::Acquire) {
                return None;
            }
            Some(guard.send(Message::Text(text.into())).await)
        };
        match tokio::time::timeout_at(deadline, write).await {
            Ok(Some(Ok(()))) => {}
            Ok(Some(Err(_))) => {
                // A write error means the socket is broken — as fatal as a
                // write timeout. Mark the connection dead and fail EVERY
                // in-flight request, so other callers don't hang awaiting a
                // reply that can never arrive over a dead sink.
                self.alive.store(false, Ordering::Release);
                self.pending.lock().await.drain();
                return Err(WebPilotError::ConnectionLost {
                    detail: format!("CDP socket write failed for {method}"),
                }
                .into());
            }
            Ok(None) => {
                self.pending.lock().await.remove(&id);
                return Err(WebPilotError::ConnectionLost {
                    detail: format!("CDP connection died before sending {method}"),
                }
                .into());
            }
            Err(_) => {
                // A write that can't drain within the deadline means the socket
                // is wedged (on loopback, backpressure for this long is not
                // "slow" — it is dead). Mark the connection dead and fail EVERY
                // in-flight request: none of them will see a reply over a stuck
                // sink, and a fresh session is relaunched on the next `open`.
                self.alive.store(false, Ordering::Release);
                self.pending.lock().await.drain();
                return Err(WebPilotError::Timeout {
                    kind: method.to_string(),
                    elapsed_ms: timeout.as_millis() as u64,
                }
                .into());
            }
        }

        let response = match tokio::time::timeout_at(deadline, rx).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => {
                self.pending.lock().await.remove(&id);
                return Err(WebPilotError::ConnectionLost {
                    detail: format!("CDP channel closed while awaiting {method}"),
                }
                .into());
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                return Err(WebPilotError::Timeout {
                    kind: method.to_string(),
                    elapsed_ms: timeout.as_millis() as u64,
                }
                .into());
            }
        };

        if let Some(error) = response.get("error") {
            // CDP "invalid params" (-32602) means the request carried bad
            // arguments — almost always agent input (a malformed URL, an
            // out-of-range value). Surface CDP's own message as a typed
            // InvalidArgument (exit 7) instead of a generic "CDP error" Other
            // (exit 1) that buries what the caller actually needs to fix.
            // Other codes are protocol/internal faults and stay Other.
            let code = error.get("code").and_then(serde_json::Value::as_i64);
            let message = error.get("message").and_then(|m| m.as_str());
            if code == Some(-32602) {
                return Err(WebPilotError::InvalidArgument {
                    detail: message.unwrap_or("invalid parameters").to_string(),
                }
                .into());
            }
            // A destroyed execution context: CDP's `-32000` plus this exact
            // message (its stable protocol wording). Surface it as the typed
            // `ContextGone` so the renderer-swap race is matched by type, never
            // by re-parsing a stringified error downstream.
            if code == Some(-32000)
                && message.is_some_and(|m| m.contains("Cannot find context with specified id"))
            {
                return Err(ContextGone.into());
            }
            anyhow::bail!("CDP error: {error}");
        }

        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Subscribe to the broadcast stream of unsolicited CDP events (e.g.,
    /// `Runtime.executionContextCreated`, `Page.frameNavigated`).
    pub fn subscribe_events(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

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
                .or_else(|| exception.pointer("/text"))
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

    pub async fn wait_for_event(
        &self,
        method: &str,
        timeout: std::time::Duration,
    ) -> Result<Value> {
        let target = method.to_string();
        self.wait_for_event_matching(method, timeout, move |event| {
            event.get("method").and_then(|v| v.as_str()) == Some(target.as_str())
        })
        .await
    }

    /// Like `wait_for_event`, but settles on the first event satisfying
    /// `predicate` rather than a bare method-name match — so a caller can ignore
    /// an event that names the right method but the wrong subject, e.g. a
    /// subframe `Page.frameNavigated` that would otherwise end a main-frame wait
    /// early. `label` names the awaited event in diagnostics only.
    pub async fn wait_for_event_matching(
        &self,
        label: &str,
        timeout: std::time::Duration,
        predicate: impl Fn(&Value) -> bool,
    ) -> Result<Value> {
        let mut rx = self.events.subscribe();
        let target = label.to_string();
        let deadline = tokio::time::Instant::now() + timeout;
        // Whether the event broadcast overflowed mid-wait (a burst larger than
        // `cdp.event_buffer`). The awaited event may have been one of the dropped
        // messages, so a subsequent deadline is NOT a confident "it never
        // happened" — it becomes an inconclusive Timeout that carries the loss,
        // never a ConnectionLost (the socket is still alive).
        let mut lagged = false;
        loop {
            // A connection that dies mid-wait must surface immediately as
            // ConnectionLost, not after the full deadline as a misleading
            // Timeout. The reader sets `alive=false` on death, but the struct
            // keeps the broadcast sender alive, so `recv()` never observes
            // `Closed` — poll the flag between events so the wait unblocks within
            // one tick of the drop. This mirrors `send`, which checks `alive`.
            if !self.alive.load(Ordering::Acquire) {
                return Err(WebPilotError::ConnectionLost {
                    detail: format!("CDP reader exited while waiting for {target}"),
                }
                .into());
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                if lagged {
                    // Events were dropped mid-wait (a burst exceeded
                    // `cdp.event_buffer`), so the awaited event may have fired
                    // and been discarded — the deadline is reached, but "it
                    // never happened" is not a safe conclusion. The socket is
                    // still alive (the alive-flag check above already caught a
                    // real drop), so this is a Timeout the agent retries — never
                    // a ConnectionLost that would have it tear down a live
                    // session. The free-form kind carries the loss so a retry
                    // can raise `cdp.event_buffer` rather than re-issue blindly.
                    return Err(WebPilotError::Timeout {
                        kind: format!(
                            "{target} — CDP event buffer overflowed; events were \
                             dropped, so the wait is inconclusive (retry, or raise \
                             cdp.event_buffer)"
                        ),
                        elapsed_ms: timeout.as_millis() as u64,
                    }
                    .into());
                }
                anyhow::bail!("Timeout waiting for {target}");
            }
            let tick = (deadline - now).min(std::time::Duration::from_millis(250));
            match tokio::time::timeout(tick, rx.recv()).await {
                Ok(Ok(event)) => {
                    if predicate(&event) {
                        return Ok(event);
                    }
                }
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => {
                    // Some events were dropped. The one we want may still arrive,
                    // so keep waiting — but remember the gap so a final timeout is
                    // reported as event loss, not a clean "never happened".
                    lagged = true;
                    continue;
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    return Err(WebPilotError::ConnectionLost {
                        detail: format!("CDP event channel closed while waiting for {target}"),
                    }
                    .into());
                }
                // Poll tick elapsed with no event — loop to re-check `alive` and
                // the deadline.
                Err(_) => continue,
            }
        }
    }

    /// Viewport screenshot (PNG, base64).
    pub async fn screenshot(&self) -> Result<String> {
        self.screenshot_inner(false).await
    }

    /// Full-page screenshot (PNG, base64). Uses CDP's `captureBeyondViewport`
    /// so we get the entire scrollable area in a single call — no tiling.
    pub async fn screenshot_full_page(&self) -> Result<String> {
        self.screenshot_inner(true).await
    }

    async fn screenshot_inner(&self, beyond: bool) -> Result<String> {
        let mut params = serde_json::json!({"format": "png"});
        if beyond {
            params["captureBeyondViewport"] = true.into();
        }
        let result = self
            .send_with_timeout(
                "Page.captureScreenshot",
                Some(params),
                std::time::Duration::from_secs(30),
            )
            .await?;
        result
            .get("data")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("No screenshot data"))
    }

    /// All cookies in the page's browser context. Use this for session export.
    /// Per `Storage.getCookies` semantics, scopes to the given `browserContextId`
    /// when provided (multi-agent isolation), or browser-wide otherwise.
    pub async fn get_all_cookies(&self, browser_context_id: Option<&str>) -> Result<Vec<Value>> {
        let params = browser_context_id.map(|id| serde_json::json!({"browserContextId": id}));
        let result = self.send("Storage.getCookies", params).await?;
        Ok(require_array(&result, "cookies", "Storage.getCookies")?.clone())
    }

    pub async fn get_targets(&self) -> Result<Vec<Value>> {
        let result = self.send("Target.getTargets", None).await?;
        Ok(require_array(&result, "targetInfos", "Target.getTargets")?.clone())
    }

    pub async fn get_browser_contexts(&self) -> Result<Vec<String>> {
        let result = self.send("Target.getBrowserContexts", None).await?;
        Ok(
            require_array(&result, "browserContextIds", "Target.getBrowserContexts")?
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        )
    }

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
            .map(str::to_string)
            .ok_or_else(|| anyhow!("No browserContextId in response"))
    }

    pub async fn dispose_browser_context(&self, browser_context_id: &str) -> Result<()> {
        self.send(
            "Target.disposeBrowserContext",
            Some(serde_json::json!({"browserContextId": browser_context_id})),
        )
        .await?;
        Ok(())
    }

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
            .map(str::to_string)
            .ok_or_else(|| anyhow!("No targetId in response"))
    }
}

/// Borrow a required array field from a CDP response. These methods always
/// carry the field on success, so a missing or wrong-typed one is a malformed
/// response — never silently an empty list. Reading it as empty would let a
/// caller act on a lie: dispose a live context whose listing came back blank,
/// or export a session reporting zero cookies as success. An array that is
/// present but empty is valid (genuinely none).
fn require_array<'a>(value: &'a Value, field: &str, method: &str) -> Result<&'a Vec<Value>> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("malformed {method} response: missing '{field}' array"))
}

impl Drop for CdpClient {
    fn drop(&mut self) {
        // Abort background tasks to prevent resource leaks.
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

/// UTF-8 safe character-bounded prefix for log previews.
fn char_safe_prefix(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Reply, bind, mock_cdp, ok};
    use tokio_tungstenite::accept_async;

    #[test]
    fn prefix_is_codepoint_safe_for_multibyte() {
        let s: String = "한".repeat(500);
        let p = char_safe_prefix(&s, 200);
        assert_eq!(p.chars().count(), 200);
    }

    #[test]
    fn prefix_does_not_overrun() {
        let s = "abc";
        assert_eq!(char_safe_prefix(s, 100), "abc");
    }

    // ── Mock-CDP harness ──────────────────────────────────────────────────
    // These drive the real `CdpClient` over the loopback mock in `test_support`,
    // so the actual reader task, id→oneshot routing, timeout, and reader-exit
    // drain are exercised end to end — not a reimplementation.

    fn webpilot_err(err: &anyhow::Error) -> &WebPilotError {
        err.downcast_ref::<WebPilotError>()
            .expect("a typed WebPilotError")
    }

    #[tokio::test]
    async fn routes_responses_to_callers_by_id_out_of_order() {
        // Read two requests, reply in REVERSE order: proves responses are
        // matched to callers by `id`, never by arrival order. (Bespoke because
        // it buffers both requests before replying — not a per-request mock.)
        let (listener, url) = bind().await;
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            let mut reqs = Vec::new();
            while reqs.len() < 2 {
                match ws.next().await {
                    Some(Ok(Message::Text(t))) => {
                        reqs.push(serde_json::from_str::<Value>(&t).unwrap())
                    }
                    _ => return,
                }
            }
            for req in reqs.into_iter().rev() {
                let resp = ok(&req, serde_json::json!({ "method": req["method"] }));
                ws.send(Message::Text(resp.to_string().into()))
                    .await
                    .unwrap();
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let cdp = CdpClient::connect(&url).await.unwrap();
        let (a, b) = tokio::join!(cdp.send("Alpha", None), cdp.send("Beta", None));
        assert_eq!(a.unwrap()["method"], "Alpha");
        assert_eq!(b.unwrap()["method"], "Beta");
    }

    #[tokio::test]
    async fn unanswered_request_times_out() {
        let url = mock_cdp(|_| Reply::Silent).await;
        let cdp = CdpClient::connect(&url).await.unwrap();
        let err = cdp
            .send_with_timeout("Hang", None, std::time::Duration::from_millis(150))
            .await
            .unwrap_err();
        assert!(matches!(webpilot_err(&err), WebPilotError::Timeout { .. }));
        assert_eq!(webpilot_err(&err).exit_code(), 5);
    }

    #[tokio::test]
    async fn closed_connection_fails_inflight_with_connection_lost() {
        let url = mock_cdp(|_| Reply::Close).await;
        let cdp = CdpClient::connect(&url).await.unwrap();
        let err = cdp.send("Doomed", None).await.unwrap_err();
        assert!(matches!(
            webpilot_err(&err),
            WebPilotError::ConnectionLost { .. }
        ));
        assert_eq!(webpilot_err(&err).exit_code(), 3);
    }

    #[tokio::test]
    async fn cdp_error_response_is_surfaced() {
        let url = mock_cdp(|req| {
            Reply::Send(serde_json::json!({
                "id": req["id"],
                "error": { "code": -32000, "message": "boom" },
            }))
        })
        .await;
        let cdp = CdpClient::connect(&url).await.unwrap();
        let err = cdp.send("Boom", None).await.unwrap_err();
        assert!(err.to_string().contains("CDP error"));
    }
}
