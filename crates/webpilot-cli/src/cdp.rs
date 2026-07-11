//! CDP (Chrome DevTools Protocol) client over WebSocket.
//!
//! One [`CdpClient`] is one WebSocket to Chrome's browser endpoint. Page
//! targets are driven through flat-protocol sessions ([`CdpSession`], from
//! [`CdpClient::attach`]): every command is stamped with its `sessionId` and
//! every session event receiver is filtered to that `sessionId`, so one
//! connection carries the browser domain and any number of page sessions
//! without their event streams bleeding into each other. Higher-level
//! abstractions (bridge injection, navigation, browser-context lifecycle)
//! live in `transport::local`.

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
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};
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

/// CDP reported that the execution context died WHILE an evaluation was in
/// flight — a navigation tore the document down under an awaited call. Distinct
/// from [`ContextGone`], where the targeted id no longer *resolved* and the call
/// never started (safe for any caller to re-issue): here the work may already
/// have run, so only an idempotent caller — the `wait` re-arm loop — may retry
/// on it. Everywhere else it surfaces as itself, which still beats the raw
/// `CDP error: {...}` blob it would otherwise collapse into.
#[derive(Debug)]
pub struct ContextDestroyedMidFlight;

impl std::fmt::Display for ContextDestroyedMidFlight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("execution context was destroyed mid-call (the document navigated)")
    }
}

impl std::error::Error for ContextDestroyedMidFlight {}

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
        // Raise tungstenite's default 16 MiB frame/message cap to a high finite
        // bound: a CDP message is TRUSTED localhost traffic from the Chrome WE
        // launched, and the 16 MiB default converts a page's own large output into
        // a session-killer (a single `console.log` over 16 MiB, replayed by
        // `Runtime.enable` on every reconnect, permanently wedged the engine with
        // `ConnectionLost`; a large `eval` return or DOM capture tripped it too).
        // The data WebPilot keeps is bounded downstream (DOM/option caps, the
        // 4096-char monitor clip, the 10 MB fetch ceiling) and the console-buffer
        // replay is cleared via `Runtime.discardConsoleEntries` before each enable,
        // so realistic traffic never approaches this; the finite cap stays only as
        // a last-resort guard against a pathological multi-GB frame OOM-ing the
        // CLI (which `None` would allow).
        const MAX_CDP_FRAME: usize = 256 << 20;
        let config = WebSocketConfig::default()
            .max_frame_size(Some(MAX_CDP_FRAME))
            .max_message_size(Some(MAX_CDP_FRAME));
        let (ws, _) = connect_async_with_config(ws_url, Some(config), false)
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
        self.send_on(
            None,
            None,
            method,
            params,
            webpilot::settings::timeouts().cdp_send,
        )
        .await
    }

    /// Send one CDP command, optionally scoped to a flat-protocol session, and
    /// await its response. This is the single dispatch path: connection-level
    /// commands pass `None`, [`CdpSession`] stamps its `sessionId` and its
    /// `session_alive` flag so a detach mid-response ends the wait at once
    /// rather than at the full deadline.
    async fn send_on(
        &self,
        session_id: Option<&str>,
        session_alive: Option<&AtomicBool>,
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
        let mut msg = serde_json::json!({
            "id": id,
            "method": method,
            "params": params.unwrap_or(Value::Object(Default::default())),
        });
        if let Some(session) = session_id {
            msg["sessionId"] = serde_json::json!(session);
        }

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

        // Await the response in short ticks so a session detach can end the wait:
        // on the shared connection a detached session (its tab closed) leaves no
        // dedicated socket to drop, so nothing would otherwise wake this wait
        // until the full deadline. The response is polled FIRST each tick; the
        // detach watcher's flag is consulted only when a tick elapses with no
        // reply. That ordering matters: if the reply and the session's detach
        // land in the same window, the delivered response still wins — the wait
        // never discards a reply already in `rx` in favour of a same-tick detach,
        // matching what the per-target socket delivered. When a tick truly passes
        // with nothing, a detached session means the reply is never coming, so we
        // drop the pending entry and surface ConnectionLost — the same typed
        // outcome the socket's death produced, so tab-gone reclassification
        // downstream is unchanged. A dead CONNECTION still resolves `rx` via the
        // reader's pending-drain, handled below.
        let mut rx = rx;
        let response = loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                self.pending.lock().await.remove(&id);
                return Err(WebPilotError::Timeout {
                    kind: method.to_string(),
                    elapsed_ms: timeout.as_millis() as u64,
                }
                .into());
            }
            let tick = (deadline - now).min(std::time::Duration::from_millis(250));
            match tokio::time::timeout(tick, &mut rx).await {
                Ok(Ok(v)) => break v,
                Ok(Err(_)) => {
                    self.pending.lock().await.remove(&id);
                    return Err(WebPilotError::ConnectionLost {
                        detail: format!("CDP channel closed while awaiting {method}"),
                    }
                    .into());
                }
                Err(_) => {
                    if session_alive.is_some_and(|a| !a.load(Ordering::Acquire)) {
                        self.pending.lock().await.remove(&id);
                        return Err(WebPilotError::ConnectionLost {
                            detail: format!("CDP session detached while awaiting {method}"),
                        }
                        .into());
                    }
                }
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
            // CDP's dedicated "session not found" code: the target this session
            // was attached to is gone (the tab closed). The dedicated per-target
            // socket used to surface this as its socket dropping; on a shared
            // connection the typed equivalent is ConnectionLost, so every
            // downstream tab-gone reclassification (`target_absent` → TabNotFound)
            // keeps working unchanged.
            if code == Some(-32001) && session_id.is_some() {
                return Err(WebPilotError::ConnectionLost {
                    detail: format!("CDP session gone while sending {method} (tab closed?)"),
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
            // The context died while THIS call was in flight (vs. never
            // resolving, above) — `-32000` plus either stable protocol wording:
            // "Execution context was destroyed" when a frame's document is
            // swapped out under the evaluation, "Inspected target navigated or
            // closed" when the TOP document navigates (measured; this is what a
            // page-initiated redirect produces). The "or closed" half is safe
            // under the same type: a re-issued call against a genuinely closed
            // target fails ConnectionLost on the next send. Typed by the same
            // rule as `ContextGone`: matched by type downstream, never by
            // re-parsing a stringified error.
            if code == Some(-32000)
                && message.is_some_and(|m| {
                    m.contains("Execution context was destroyed")
                        || m.contains("Inspected target navigated or closed")
                })
            {
                return Err(ContextDestroyedMidFlight.into());
            }
            anyhow::bail!("CDP error: {error}");
        }

        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Subscribe to the broadcast stream of unsolicited CDP events. This is the
    /// CONNECTION-level view — browser-domain events (`Target.targetCreated`)
    /// plus every attached session's events. Session-scoped consumers use
    /// [`CdpSession::subscribe_events`], which filters to one `sessionId`.
    pub fn subscribe_events(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    /// Attach a flat-protocol session to `target_id`. The session shares this
    /// connection: its commands are stamped with the returned `sessionId` and
    /// its event receivers see only that session's events.
    pub async fn attach(self: &Arc<Self>, target_id: &str) -> Result<CdpSession> {
        let result = self
            .send(
                "Target.attachToTarget",
                Some(serde_json::json!({ "targetId": target_id, "flatten": true })),
            )
            .await?;
        let session_id = result
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Target.attachToTarget returned no sessionId"))?;

        let session_alive = Arc::new(AtomicBool::new(true));
        // Mark the session dead the moment Chrome announces its detachment (the
        // tab closed, or an explicit detach), so in-flight waits unblock within
        // one poll tick instead of running to their deadline. The `-32001`
        // mapping in `send_on` is the backstop for a detach event that lagged
        // out of the ring. The watcher ends itself on that same signal.
        let mut watch = SessionEvents {
            rx: self.subscribe_events(),
            session_id: session_id.clone(),
        };
        let alive = session_alive.clone();
        let watcher = tokio::spawn(async move {
            loop {
                match watch.recv().await {
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        alive.store(false, Ordering::Release);
                        break;
                    }
                }
            }
        });

        Ok(CdpSession {
            conn: self.clone(),
            session_id,
            session_alive,
            background: vec![watcher],
        })
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

    /// Create a page target, scoped to `browser_context_id` when given (the
    /// default browser context otherwise). One creation API for `tab new` and
    /// the zero-page attach, so context scoping can't drift between them.
    pub async fn create_target(
        &self,
        url: &str,
        browser_context_id: Option<&str>,
    ) -> Result<String> {
        let mut params = serde_json::json!({ "url": url });
        if let Some(ctx) = browser_context_id {
            params["browserContextId"] = serde_json::json!(ctx);
        }
        let result = self.send("Target.createTarget", Some(params)).await?;
        result
            .get("targetId")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("No targetId in response"))
    }
}

/// The inconclusive form of a wait deadline: events were dropped mid-wait, so
/// "it never happened" is not a safe conclusion. The socket is still alive, so
/// this is a Timeout the agent retries — never a ConnectionLost that would have
/// it tear down a live session. The free-form kind carries the loss so a retry
/// can raise `cdp.event_buffer` rather than re-issue blindly.
fn inconclusive_wait_timeout(target: &str, timeout: std::time::Duration) -> anyhow::Error {
    WebPilotError::Timeout {
        kind: format!(
            "{target} — CDP event buffer overflowed; events were \
             dropped, so the wait is inconclusive (retry, or raise \
             cdp.event_buffer)"
        ),
        elapsed_ms: timeout.as_millis() as u64,
    }
    .into()
}

/// A flat-protocol session to one target, sharing its [`CdpClient`]
/// connection. Mirrors the page-facing API a dedicated per-target socket used
/// to provide: sends are stamped with the `sessionId`, event receivers are
/// filtered to it, and a detached session (its tab closed) surfaces as the
/// same typed `ConnectionLost` a dropped socket did.
pub struct CdpSession {
    conn: Arc<CdpClient>,
    session_id: String,
    session_alive: Arc<AtomicBool>,
    /// Every task this session spawns onto the shared connection: the detach
    /// watcher (`attach`), the dialog responder (`spawn_dialog_responder`), and
    /// the frame-context listener (handed in by the transport via `track`).
    /// `Drop` aborts them all, so none can outlive the session it belongs to.
    /// For the dialog responder this is load-bearing, not hygiene: it holds an
    /// `Arc<CdpClient>`, so a responder parked on `recv` would pin the whole
    /// connection — its `events` sender never drops, so `recv` never returns
    /// `Closed`, so the task never exits on its own once Chrome is dead. Without
    /// the abort, each Chrome-death→reopen cycle in a long-lived `webpilot mcp`
    /// server would leak a `CdpClient` and its reader/heartbeat tasks. The
    /// watcher and listener hold no such Arc, but tracking them here keeps every
    /// per-session task on one deterministic teardown instead of relying on a
    /// detach event that a wedged-then-recovered connection could drop.
    background: Vec<JoinHandle<()>>,
}

impl CdpSession {
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
        if !self.session_alive.load(Ordering::Acquire) {
            return Err(WebPilotError::ConnectionLost {
                detail: format!("CDP session detached before sending {method}"),
            }
            .into());
        }
        self.conn
            .send_on(
                Some(&self.session_id),
                Some(&self.session_alive),
                method,
                params,
                timeout,
            )
            .await
    }

    /// Hand a task spawned for this session (e.g. the frame-context listener,
    /// spawned by the transport with the maps it feeds) to the session so `Drop`
    /// aborts it — the same deterministic teardown the detach watcher and dialog
    /// responder get, so no per-session task outlives the session.
    pub fn track(&mut self, task: JoinHandle<()>) {
        self.background.push(task);
    }

    /// Subscribe to this session's events only. Browser-domain events and other
    /// sessions' events never pass the filter, reproducing the isolation a
    /// dedicated per-target socket gave — a settle drain can never consume
    /// another page's `Page.frameStartedLoading`.
    pub fn subscribe_events(&self) -> SessionEvents {
        SessionEvents {
            rx: self.conn.subscribe_events(),
            session_id: self.session_id.clone(),
        }
    }

    /// Auto-answer page javascript dialogs (alert/confirm/prompt/beforeunload).
    ///
    /// With `Page` enabled on a session, Chrome STOPS its headless auto-dismiss
    /// and waits for `Page.handleJavaScriptDialog` — an unanswered `alert()`
    /// would wedge the renderer, timing out every later command on the page.
    /// Accept with the prompt's default, matching the browser-mode dialog
    /// override (confirm → true, prompt → its default stringified), so page
    /// flows branching on a dialog proceed identically in both modes.
    ///
    /// The answer is fire-and-forget (`send_on` with a short deadline, result
    /// ignored): a failed send means the session or connection is dead anyway.
    /// The task's handle is retained so `Drop` reaps it — it holds an
    /// `Arc<CdpClient>` and so cannot be relied on to exit by itself once Chrome
    /// is dead (see the `background` field).
    pub fn spawn_dialog_responder(&mut self) {
        let mut events = self.subscribe_events();
        let conn = self.conn.clone();
        let session_id = self.session_id.clone();
        let handle = tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(ev) => {
                        if ev.get("method").and_then(Value::as_str)
                            != Some("Page.javascriptDialogOpening")
                        {
                            continue;
                        }
                        let prompt_text = ev
                            .pointer("/params/defaultPrompt")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let _ = conn
                            .send_on(
                                Some(&session_id),
                                None,
                                "Page.handleJavaScriptDialog",
                                Some(serde_json::json!({
                                    "accept": true,
                                    "promptText": prompt_text,
                                })),
                                std::time::Duration::from_secs(HEARTBEAT_TIMEOUT_S),
                            )
                            .await;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        self.background.push(handle);
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

    /// Like `wait_for_event`, but settles on the first of THIS SESSION's events
    /// satisfying `predicate` rather than a bare method-name match — so a caller
    /// can ignore an event that names the right method but the wrong subject,
    /// e.g. a subframe `Page.frameNavigated` that would otherwise end a
    /// main-frame wait early. `label` names the awaited event in diagnostics only.
    pub async fn wait_for_event_matching(
        &self,
        label: &str,
        timeout: std::time::Duration,
        predicate: impl Fn(&Value) -> bool,
    ) -> Result<Value> {
        let mut rx = self.subscribe_events();
        let target = label.to_string();
        let deadline = tokio::time::Instant::now() + timeout;
        let mut lagged = false;
        loop {
            // A session or connection that dies mid-wait must surface
            // immediately as ConnectionLost, not after the full deadline as a
            // misleading Timeout — the same contract the dedicated per-target
            // socket's death gave. Polled between events, so the wait unblocks
            // within one tick.
            if !self.conn.alive.load(Ordering::Acquire)
                || !self.session_alive.load(Ordering::Acquire)
            {
                return Err(WebPilotError::ConnectionLost {
                    detail: format!("CDP session ended while waiting for {target}"),
                }
                .into());
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                if lagged {
                    return Err(inconclusive_wait_timeout(&target, timeout));
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
                    lagged = true;
                    continue;
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => {
                    return Err(WebPilotError::ConnectionLost {
                        detail: format!("CDP session ended while waiting for {target}"),
                    }
                    .into());
                }
                Err(_) => continue,
            }
        }
    }

    /// Wait, on an EXISTING session subscription, for an event matching
    /// `predicate`, returning whether one arrived before `timeout`. The caller
    /// subscribes *before* triggering the action, so an event fired between the
    /// trigger and the wait cannot slip through. Yields a plain outcome bool: a
    /// dead session/connection or a dropped-event overflow ends the wait as a
    /// non-arrival, and the caller confirms the negative another way.
    pub async fn wait_on_receiver(
        &self,
        rx: &mut SessionEvents,
        timeout: std::time::Duration,
        predicate: impl Fn(&Value) -> bool,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if !self.conn.alive.load(Ordering::Acquire)
                || !self.session_alive.load(Ordering::Acquire)
            {
                return false;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return false;
            }
            let tick = (deadline - now).min(std::time::Duration::from_millis(250));
            match tokio::time::timeout(tick, rx.recv()).await {
                Ok(Ok(event)) if predicate(&event) => return true,
                Ok(Ok(_)) | Ok(Err(broadcast::error::RecvError::Lagged(_))) | Err(_) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => return false,
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
}

impl Drop for CdpSession {
    fn drop(&mut self) {
        // Stop every task this session spawned, deterministically — none can
        // outlive the session it belongs to. This is what makes the dialog
        // responder's `Arc<CdpClient>` pin harmless: aborting it releases that
        // Arc so the connection can actually drop, and it covers the detach
        // watcher too when its detach event never arrives (lag, or a shutdown
        // that races it).
        for task in &self.background {
            task.abort();
        }
        self.session_alive.store(false, Ordering::Release);
        // Detach so a replaced session (a tab switch in a long-lived process)
        // stops streaming its events into the shared connection. Fire-and-forget:
        // a process exiting closes the connection, which detaches everything
        // anyway, and outside a runtime there is no task to spawn.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let conn = self.conn.clone();
            let session_id = self.session_id.clone();
            handle.spawn(async move {
                let _ = conn
                    .send_on(
                        None,
                        None,
                        "Target.detachFromTarget",
                        Some(serde_json::json!({ "sessionId": session_id })),
                        std::time::Duration::from_secs(HEARTBEAT_TIMEOUT_S),
                    )
                    .await;
            });
        }
    }
}

/// A [`CdpSession`]-scoped event receiver: `recv` yields only events carrying
/// this session's `sessionId`, and translates the session's own
/// `Target.detachedFromTarget` into `Closed` — so every consumer loop ends the
/// way it did when the per-target socket closed, instead of parking forever on
/// a stream that will never speak for this session again.
pub struct SessionEvents {
    rx: broadcast::Receiver<Value>,
    session_id: String,
}

impl SessionEvents {
    /// Non-blocking drain counterpart of `recv`, with the same session filter
    /// and detach translation — for callers replaying events already buffered
    /// before an action's response arrived.
    pub fn try_recv(&mut self) -> Result<Value, broadcast::error::TryRecvError> {
        loop {
            let event = self.rx.try_recv()?;
            if event.get("sessionId").and_then(Value::as_str) == Some(self.session_id.as_str()) {
                return Ok(event);
            }
            if event.get("method").and_then(Value::as_str) == Some("Target.detachedFromTarget")
                && event.pointer("/params/sessionId").and_then(Value::as_str)
                    == Some(self.session_id.as_str())
            {
                return Err(broadcast::error::TryRecvError::Closed);
            }
        }
    }

    pub async fn recv(&mut self) -> Result<Value, broadcast::error::RecvError> {
        loop {
            let event = self.rx.recv().await?;
            if event.get("sessionId").and_then(Value::as_str) == Some(self.session_id.as_str()) {
                return Ok(event);
            }
            if event.get("method").and_then(Value::as_str) == Some("Target.detachedFromTarget")
                && event.pointer("/params/sessionId").and_then(Value::as_str)
                    == Some(self.session_id.as_str())
            {
                return Err(broadcast::error::RecvError::Closed);
            }
        }
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
    use crate::test_support::{Reply, attach_session, bind, mock_cdp, ok};
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
        let url = mock_cdp(|req| match req["method"].as_str() {
            Some("Target.attachToTarget") => {
                Reply::Send(ok(req, serde_json::json!({ "sessionId": "S1" })))
            }
            _ => Reply::Silent,
        })
        .await;
        let conn = Arc::new(CdpClient::connect(&url).await.unwrap());
        let session = attach_session(&conn).await;
        let err = session
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

    // ── Flat sessions ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn session_commands_carry_the_session_id() {
        // The mock echoes the request's sessionId back in the result, so the
        // assertion proves the stamped id crossed the wire — not that the
        // client remembered it locally.
        let url = mock_cdp(|req| match req["method"].as_str() {
            Some("Target.attachToTarget") => {
                Reply::Send(ok(req, serde_json::json!({ "sessionId": "S1" })))
            }
            _ => Reply::Send(ok(
                req,
                serde_json::json!({ "sawSession": req["sessionId"] }),
            )),
        })
        .await;
        let conn = Arc::new(CdpClient::connect(&url).await.unwrap());
        let session = conn.attach("T1").await.unwrap();
        let result = session.send("Page.enable", None).await.unwrap();
        assert_eq!(result["sawSession"], "S1");
        // A connection-level send carries no sessionId.
        let result = conn.send("Browser.getVersion", None).await.unwrap();
        assert_eq!(result["sawSession"], Value::Null);
    }

    #[tokio::test]
    async fn session_events_are_filtered_to_the_session() {
        // A request's reply is preceded by three unsolicited events: another
        // session's, a connection-level one, and ours. Only ours reaches the
        // session receiver — the isolation the per-target socket used to give.
        let url = mock_cdp(|req| match req["method"].as_str() {
            Some("Target.attachToTarget") => {
                Reply::Send(ok(req, serde_json::json!({ "sessionId": "S1" })))
            }
            Some("Trigger") => Reply::SendAll(vec![
                serde_json::json!({ "method": "Page.loadEventFired", "params": {}, "sessionId": "OTHER" }),
                serde_json::json!({ "method": "Target.targetCreated", "params": {} }),
                serde_json::json!({ "method": "Page.loadEventFired", "params": {}, "sessionId": "S1" }),
                ok(req, serde_json::json!({})),
            ]),
            _ => Reply::Send(ok(req, serde_json::json!({}))),
        })
        .await;
        let conn = Arc::new(CdpClient::connect(&url).await.unwrap());
        let session = attach_session(&conn).await;
        let mut events = session.subscribe_events();
        session.send("Trigger", None).await.unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
            .await
            .expect("event within deadline")
            .expect("an event, not closed");
        assert_eq!(ev["method"], "Page.loadEventFired");
        assert_eq!(ev["sessionId"], "S1");
    }

    #[tokio::test]
    async fn detach_event_closes_session_receivers() {
        let url = mock_cdp(|req| match req["method"].as_str() {
            Some("Target.attachToTarget") => {
                Reply::Send(ok(req, serde_json::json!({ "sessionId": "S1" })))
            }
            Some("Trigger") => Reply::SendAll(vec![
                serde_json::json!({
                    "method": "Target.detachedFromTarget",
                    "params": { "sessionId": "S1" },
                }),
                ok(req, serde_json::json!({})),
            ]),
            _ => Reply::Send(ok(req, serde_json::json!({}))),
        })
        .await;
        let conn = Arc::new(CdpClient::connect(&url).await.unwrap());
        let session = attach_session(&conn).await;
        let mut events = session.subscribe_events();
        session.send("Trigger", None).await.unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
            .await
            .expect("recv settles within deadline");
        assert!(
            matches!(outcome, Err(broadcast::error::RecvError::Closed)),
            "a detached session's receiver must close, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn session_gone_error_maps_to_connection_lost() {
        // Chrome's dedicated -32001 "session not found" answer (the tab closed
        // under us) must surface as the same typed ConnectionLost the dedicated
        // socket's drop used to produce, so tab-gone reclassification holds.
        let url = mock_cdp(|req| match req["method"].as_str() {
            Some("Target.attachToTarget") => {
                Reply::Send(ok(req, serde_json::json!({ "sessionId": "S1" })))
            }
            _ => Reply::Send(serde_json::json!({
                "id": req["id"],
                "error": { "code": -32001, "message": "Session with given id not found." },
            })),
        })
        .await;
        let conn = Arc::new(CdpClient::connect(&url).await.unwrap());
        let session = attach_session(&conn).await;
        let err = session.send("Page.enable", None).await.unwrap_err();
        assert!(matches!(
            webpilot_err(&err),
            WebPilotError::ConnectionLost { .. }
        ));
    }

    #[tokio::test]
    async fn dropping_a_session_releases_the_connection_even_with_a_dialog_responder() {
        // The dialog responder holds an Arc<CdpClient>; if Drop did not abort it,
        // it would park on `recv` forever (its own Arc keeps the `events` sender
        // alive, so `recv` never returns Closed) and pin the connection. Assert
        // the session's Arc is the ONLY extra strong ref after spawning the
        // responder, and that dropping the session returns the connection to
        // uniquely-owned — proof no spawned task retains a clone.
        let url = mock_cdp(|req| match req["method"].as_str() {
            Some("Target.attachToTarget") => {
                Reply::Send(ok(req, serde_json::json!({ "sessionId": "S1" })))
            }
            _ => Reply::Send(ok(req, serde_json::json!({}))),
        })
        .await;
        let conn = Arc::new(CdpClient::connect(&url).await.unwrap());
        let mut session = conn.attach("T1").await.unwrap();
        session.spawn_dialog_responder();
        // Let the spawned tasks reach their `recv` await, cloning their Arcs.
        tokio::task::yield_now().await;
        drop(session);
        // The detach task Drop spawns also clones the Arc; give it and the
        // aborted tasks a moment to run their abort/short-circuit and release.
        for _ in 0..50 {
            if Arc::strong_count(&conn) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            Arc::strong_count(&conn),
            1,
            "dropping the session must release every task's Arc<CdpClient>, or the connection leaks"
        );
    }
}
