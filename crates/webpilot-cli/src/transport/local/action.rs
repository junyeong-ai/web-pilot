//! Page-mutating actions: dispatch, drag, navigation passthrough.
//!
//! All bridge-routed actions forward the typed `Action` directly to
//! `bridge.js`'s `executeAction` handler; CDP-level actions (navigate, drag,
//! history, reload) are handled inline against the page CDP connection.

use anyhow::Result;
use serde_json::{Value, json};
use webpilot::protocol::ResponseData;
use webpilot::{Action, WebPilotError};

use super::{LocalTransport, action_success};

/// History traversal direction — typed so the probe and the page expression
/// derive from one discriminant instead of string inspection.
#[derive(Clone, Copy)]
enum HistoryNav {
    Back,
    Forward,
}

/// Whether a URL names a settled, capturable document rather than the initial
/// empty page a tab carries before its first real navigation commits.
fn is_real_document_url(url: &str) -> bool {
    !url.is_empty() && url != "about:blank"
}

impl LocalTransport {
    // Policy is enforced once at the transport boundary (`Transport::send`),
    // so handlers run only after the command is permitted.
    pub(super) async fn do_action(
        &mut self,
        action: Action,
        capture: bool,
    ) -> Result<ResponseData> {
        match &action {
            Action::Navigate { url } => {
                let url = url.clone();
                self.navigate_reconnect(&url).await?;
                // Report where the navigation actually landed (after redirects).
                let landed = self.bound_target_url().await;
                return Ok(ResponseData::Action {
                    success: true,
                    error: None,
                    dom: None,
                    url_changed: (!landed.is_empty()).then_some(landed),
                    new_tab: None,
                    capture_error: None,
                });
            }
            Action::Back => {
                self.history_nav(HistoryNav::Back).await?;
                return Ok(action_success(None));
            }
            Action::Forward => {
                self.history_nav(HistoryNav::Forward).await?;
                return Ok(action_success(None));
            }
            Action::Reload => {
                self.page.send("Page.reload", None).await?;
                self.page
                    .wait_for_event(
                        "Page.loadEventFired",
                        webpilot::settings::timeouts().reload_wait,
                    )
                    .await
                    .ok();
                self.clear_active_frame().await;
                self.reinstall_monitors().await;
                return Ok(action_success(None));
            }
            Action::Drag {
                source,
                target,
                steps,
            } => {
                self.require_main_frame("drag").await?;
                self.do_drag(*source, *target, *steps).await?;
                return Ok(action_success(None));
            }
            Action::Hover { index } => {
                // Browser-input mouse move so CSS `:hover` actually fires.
                // Bridge.js dispatchEvent only triggers JS listeners, not the
                // internal hover state.
                self.require_main_frame("hover").await?;
                self.do_hover(*index).await?;
                return Ok(action_success(None));
            }
            Action::Upload { index, path } => {
                self.require_main_frame("upload").await?;
                let path = path.clone();
                self.do_upload(*index, &path).await?;
                return Ok(action_success(None));
            }
            _ => {}
        }

        // Mirror browser mode's action contract (`dispatchActionToPage`): the
        // popup watch and the navigation watch open BEFORE the action runs —
        // no detection window — and the URL comparison brackets the action.
        // Both are best-effort by design in both modes: a navigation or popup
        // the browser registers after the action's round-trip is reported by
        // the next capture.
        let url_before = self.bound_target_url().await;
        let mut target_events = self.browser.subscribe_events();
        let mut page_events = self.page.subscribe_events();

        let action_json = serde_json::to_value(&action)?;
        let raw = self
            .invoke_bridge(&json!({"type": "executeAction", "action": action_json}))
            .await?;
        let _ = Self::parse_bridge_response(raw)?;

        // Sample the acted-on page before any pin move, so `url_changed`
        // reports this tab's navigation, never the popup's URL.
        let landed = self.settled_action_url(&mut page_events, &url_before).await;
        let url_changed = (!landed.is_empty() && landed != url_before).then_some(landed);
        if url_changed.is_some() {
            // The action landed on a new document: a frame scope switched in
            // the old document died with it (same contract as `navigate`),
            // and the `window` monitor hooks are gone — the commit has been
            // observed, so re-arm here. The CDP page session itself survives
            // a cross-site renderer swap (the /devtools/page endpoint lives
            // browser-side — verified against a file://→http:// process
            // swap), so no session rebind is needed. A navigation that
            // outlives the bounded settle window resumes recording at the
            // next WebPilot navigation or tab command.
            self.clear_active_frame().await;
            self.reinstall_monitors().await;
        }

        // Adopt a click-opened tab BEFORE the capture: the pin moves to the
        // popup (the browser-mode contract), and the agent's snapshot must
        // describe the tab it will act on next — capturing the opener would
        // hand back indices that resolve nowhere.
        let new_tab = self.adopt_click_opened_target(&mut target_events).await;

        // Capture AFTER everything settled: for a navigating click that is
        // the committed-and-parsed new document, for a popup the adopted tab.
        // A capture failure must not fail the command — the action's side
        // effect is done, and a retry would run it twice — so it is reported
        // alongside the success as `capture_error`.
        let (dom, capture_error) = if capture {
            if new_tab.is_some() {
                // Fresh subscription: readiness of the adopted tab, not the
                // opener that `page_events` is bound to. A popup often exists
                // as `about:blank` (readyState complete) before its
                // destination commits — wait past the blank document first so
                // the snapshot is the page the click actually opened.
                let mut popup_events = self.page.subscribe_events();
                self.await_adopted_document(&mut popup_events).await;
            } else if url_changed.is_some() {
                self.await_document_ready(&mut page_events).await;
            }
            match self.capture_action_snapshot().await {
                Ok(snapshot) => (Some(snapshot), None),
                Err(e) => (None, Some(e.to_string())),
            }
        } else {
            (None, None)
        };

        Ok(ResponseData::Action {
            success: true,
            error: None,
            dom,
            url_changed,
            new_tab,
            capture_error,
        })
    }

    /// One post-action DOM snapshot from the currently bound page.
    async fn capture_action_snapshot(&self) -> Result<webpilot::types::DomSnapshot> {
        let r = self
            .invoke_bridge(&json!({"type": "extractDom", "options": {}}))
            .await?;
        let r = Self::parse_bridge_response(r)?;
        let mut snapshot: webpilot::types::DomSnapshot =
            serde_json::from_value(r).map_err(|e| WebPilotError::Other {
                detail: format!("malformed DOM snapshot from bridge: {e}"),
            })?;
        if self.active_frame_id.lock().await.is_none() {
            snapshot.subframes = self.count_http_subframes().await;
        }
        Ok(snapshot)
    }

    /// Typed guard for actions whose CDP path (page-viewport coordinates or
    /// main-document node lookup) cannot target an iframe. Inside a switched
    /// frame these would silently act on the wrong position — fail loudly
    /// instead.
    async fn require_main_frame(&self, kind: &str) -> Result<()> {
        if self.active_frame_id.lock().await.is_some() {
            return Err(WebPilotError::InvalidArgument {
                detail: format!(
                    "'{kind}' targets the main frame only and an iframe is active. Switch back first: webpilot frame switch main"
                ),
            }
            .into());
        }
        Ok(())
    }

    async fn do_upload(&self, index: u32, path: &std::path::Path) -> Result<()> {
        // Bridge tags the chosen <input type=file>; CDP then sets the file
        // by NodeId because file inputs cannot be filled by JS.
        let tag = self
            .invoke_bridge(&json!({
                "type": "tagElement",
                "index": index,
                "attr": "data-wp-upload",
            }))
            .await?;
        let _ = Self::parse_bridge_response(tag)?;

        let doc = self.page.send("DOM.getDocument", None).await?;
        let root = doc
            .pointer("/root/nodeId")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| WebPilotError::Other {
                detail: "DOM.getDocument returned no root nodeId".into(),
            })?;

        let q = self
            .page
            .send(
                "DOM.querySelector",
                Some(json!({
                    "nodeId": root,
                    "selector": "[data-wp-upload]",
                })),
            )
            .await?;
        let node_id = q.get("nodeId").and_then(|v| v.as_i64()).filter(|n| *n != 0);
        let Some(node_id) = node_id else {
            // Best-effort cleanup, then surface a typed error.
            let _ = self
                .invoke_bridge(&json!({"type": "untagElement", "attr": "data-wp-upload"}))
                .await;
            return Err(WebPilotError::ElementNotFound {
                requested: index,
                available: 0,
            }
            .into());
        };

        let set = self
            .page
            .send(
                "DOM.setFileInputFiles",
                Some(json!({
                    "nodeId": node_id,
                    "files": [path.to_string_lossy()],
                })),
            )
            .await;

        // Remove the marker whether or not the file assignment succeeded, so a
        // failed upload never leaves a stale attribute on the input.
        let _ = self
            .invoke_bridge(&json!({"type": "untagElement", "attr": "data-wp-upload"}))
            .await;
        set?;
        Ok(())
    }

    async fn do_hover(&self, index: u32) -> Result<()> {
        // Resolve the visible-element center via bridge (uses the same
        // index space that capture exposes), then move the cursor there
        // through CDP's input layer. This is what activates `:hover`.
        let coords = self
            .invoke_bridge(&serde_json::json!({
                "type": "getElementCoords",
                "source": index,
                "target": index,
            }))
            .await?;
        let resp = Self::parse_bridge_response(coords)?;
        let x = coord(&resp, "sx")?;
        let y = coord(&resp, "sy")?;
        self.page
            .send(
                "Input.dispatchMouseEvent",
                Some(json!({"type": "mouseMoved", "x": x, "y": y, "button": "none"})),
            )
            .await?;
        Ok(())
    }

    /// Where the acted-on page's main frame ended up after an action, given
    /// the page events buffered since just before the action ran.
    ///
    /// The immediate URL compare covers navigations that committed within the
    /// action's round-trip (and same-document/SPA route changes, which update
    /// the target URL synchronously). When the action *started* a main-frame
    /// load that has not committed yet — a plain link click racing the bridge
    /// response — the commit is awaited, bounded by `PROBE`, so the agent gets
    /// `url_changed` exactly as browser mode reports it. An action that
    /// started no main-frame load pays nothing: no events, no wait.
    async fn settled_action_url(
        &self,
        events: &mut tokio::sync::broadcast::Receiver<Value>,
        before: &str,
    ) -> String {
        use tokio::sync::broadcast::error::{RecvError, TryRecvError};

        fn main_frame_url(ev: &Value) -> Option<&str> {
            if ev.get("method").and_then(Value::as_str) != Some("Page.frameNavigated") {
                return None;
            }
            let frame = ev.pointer("/params/frame")?;
            if frame.get("parentId").is_some() {
                return None;
            }
            frame.get("url").and_then(Value::as_str)
        }

        let immediate = self.bound_target_url().await;
        if !immediate.is_empty() && immediate != before {
            return immediate;
        }

        // Drain the buffer into an ordered list of the events that bear on the
        // main frame's load state. A non-navigating action produces none — the
        // hot path returns here with no extra round-trip.
        enum Ev {
            Commit(String),
            Started(String),
            Stopped(String),
        }
        let mut seq: Vec<Ev> = Vec::new();
        loop {
            match events.try_recv() {
                Ok(ev) => {
                    if let Some(u) = main_frame_url(&ev) {
                        seq.push(Ev::Commit(u.to_string()));
                    } else if let Some(id) = ev.pointer("/params/frameId").and_then(Value::as_str) {
                        match ev.get("method").and_then(Value::as_str) {
                            Some("Page.frameStartedLoading") => seq.push(Ev::Started(id.to_string())),
                            Some("Page.frameStoppedLoading") => seq.push(Ev::Stopped(id.to_string())),
                            _ => {}
                        }
                    }
                }
                Err(TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
        if seq.is_empty() {
            return immediate;
        }

        // Something loaded — now learn the main frame id to interpret the
        // sequence. A start/stop pair only settles the MAIN frame, and replay
        // IN ORDER (not set membership) is what makes `[priorStop, ourStart]`
        // resolve to "still loading" instead of short-circuiting on the stale
        // stop. (Unreadable tree → don't wait.)
        let Ok(tree) = self.page.send("Page.getFrameTree", None).await else {
            return immediate;
        };
        let Some(main) = tree
            .pointer("/frameTree/frame/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return immediate;
        };
        let mut committed: Option<String> = None;
        let mut main_loading = false;
        for e in seq {
            match e {
                Ev::Commit(u) => {
                    committed = Some(u);
                    main_loading = false;
                }
                Ev::Started(id) if id == main => main_loading = true,
                Ev::Stopped(id) if id == main => main_loading = false,
                _ => {}
            }
        }
        if let Some(u) = committed {
            return u;
        }
        if !main_loading {
            return immediate;
        }

        let deadline = tokio::time::Instant::now() + super::PROBE;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return self.bound_target_url().await;
            }
            match tokio::time::timeout(deadline - now, events.recv()).await {
                Ok(Ok(ev)) => {
                    if let Some(u) = main_frame_url(&ev) {
                        return u.to_string();
                    }
                    // The load ended without a commit (cancelled, download):
                    // nothing further to wait for.
                    if ev.get("method").and_then(Value::as_str)
                        == Some("Page.frameStoppedLoading")
                    {
                        return self.bound_target_url().await;
                    }
                }
                Ok(Err(RecvError::Lagged(_))) => continue,
                Ok(Err(_)) | Err(_) => return self.bound_target_url().await,
            }
        }
    }

    /// Wait — bounded by `PROBE` — until the committed document has parsed,
    /// mirroring the `ready` half of the navigation predicate: a capture
    /// during `readyState=loading` would hand the agent a near-empty DOM as
    /// THE result of its action. Event-driven via the already-open page
    /// subscription (`Page.domContentEventFired` is buffered from before the
    /// action), so no polling.
    async fn await_document_ready(&self, events: &mut tokio::sync::broadcast::Receiver<Value>) {
        use tokio::sync::broadcast::error::RecvError;
        if let Ok(v) = self.page.evaluate("document.readyState").await
            && v.as_str().is_some_and(|s| s != "loading")
        {
            return;
        }
        let deadline = tokio::time::Instant::now() + super::PROBE;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return;
            }
            match tokio::time::timeout(deadline - now, events.recv()).await {
                Ok(Ok(ev)) => {
                    // The event is only a wake-up signal — a buffered firing
                    // from the PREVIOUS document must not satisfy the wait, so
                    // readyState stays the authority.
                    if ev.get("method").and_then(Value::as_str)
                        == Some("Page.domContentEventFired")
                        && let Ok(v) = self.page.evaluate("document.readyState").await
                        && v.as_str().is_some_and(|s| s != "loading")
                    {
                        return;
                    }
                }
                Ok(Err(RecvError::Lagged(_))) => continue,
                Ok(Err(_)) | Err(_) => return,
            }
        }
    }

    /// Wait — bounded by `PROBE` — for a freshly adopted popup to settle on
    /// the document the click actually opened. A click-opened target commonly
    /// exists first as `about:blank` (readyState already `complete`) before
    /// its destination commits; a plain `await_document_ready` would capture
    /// that blank page. So: if the bound document is still `about:blank`, wait
    /// for the main frame to commit to a real URL, then for it to parse. A
    /// popup genuinely opened to `about:blank` settles at the deadline (its
    /// real, blank result).
    async fn await_adopted_document(&self, events: &mut tokio::sync::broadcast::Receiver<Value>) {
        use tokio::sync::broadcast::error::RecvError;

        fn main_committed_real_url(ev: &Value) -> bool {
            if ev.get("method").and_then(Value::as_str) != Some("Page.frameNavigated") {
                return false;
            }
            let Some(frame) = ev.pointer("/params/frame") else {
                return false;
            };
            if frame.get("parentId").is_some() {
                return false;
            }
            frame
                .get("url")
                .and_then(Value::as_str)
                .is_some_and(is_real_document_url)
        }

        if is_real_document_url(&self.bound_target_url().await) {
            self.await_document_ready(events).await;
            return;
        }

        let deadline = tokio::time::Instant::now() + super::PROBE;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            match tokio::time::timeout(deadline - now, events.recv()).await {
                Ok(Ok(ev)) if main_committed_real_url(&ev) => break,
                Ok(Ok(_)) => continue,
                Ok(Err(RecvError::Lagged(_))) => continue,
                Ok(Err(_)) | Err(_) => return,
            }
        }
        self.await_document_ready(events).await;
    }

    /// Drive a same-document history navigation (`history.back()`/`forward()`).
    ///
    /// The navigation tears down the execution context the expression runs in,
    /// so the evaluate's own response can come back as a CDP "target navigated"
    /// teardown — that is the success path, not a failure. Only typed transport
    /// failures (`ConnectionLost`/`Timeout`) are propagated; the navigation is
    /// then confirmed via the frame event and the active frame is reset, just
    /// as `navigate_reconnect` does.
    async fn history_nav(&self, direction: HistoryNav) -> Result<()> {
        let (probe, expression) = match direction {
            HistoryNav::Back => ("navigation.canGoBack", "history.back()"),
            HistoryNav::Forward => ("navigation.canGoForward", "history.forward()"),
        };
        // The Navigation API makes a missing history entry an honest, immediate
        // typed failure — never a success that silently did nothing (browser
        // mode applies the same check). A probe that cannot resolve (the API
        // absent) falls through to attempting the traversal: undeterminable is
        // not the same as impossible.
        if let Ok(can) = self.page.evaluate(probe).await
            && can == serde_json::Value::Bool(false)
        {
            return Err(WebPilotError::NavigationFailed {
                url: expression.to_string(),
                reason: "no history entry".into(),
            }
            .into());
        }

        if let Err(e) = self.page.evaluate(expression).await
            && matches!(
                e.downcast_ref::<WebPilotError>(),
                Some(WebPilotError::ConnectionLost { .. } | WebPilotError::Timeout { .. })
            )
        {
            return Err(e);
        }
        self.page
            .wait_for_event(
                "Page.frameNavigated",
                webpilot::settings::timeouts().back_forward,
            )
            .await
            .ok();
        self.clear_active_frame().await;
        // A history traversal that built a new document wiped the monitor
        // hooks; for a same-document traversal the re-install is an idempotent
        // no-op (the install scripts guard on their `window` flags).
        self.reinstall_monitors().await;
        Ok(())
    }

    async fn do_drag(&self, source: u32, target: u32, steps: u32) -> Result<()> {
        let coords = self
            .invoke_bridge(&json!({
                "type": "getElementCoords",
                "source": source,
                "target": target,
            }))
            .await?;
        let resp = Self::parse_bridge_response(coords)?;

        let sx = coord(&resp, "sx")?;
        let sy = coord(&resp, "sy")?;
        let tx = coord(&resp, "tx")?;
        let ty = coord(&resp, "ty")?;

        self.page
            .send(
                "Input.dispatchMouseEvent",
                Some(json!({
                    "type": "mousePressed", "x": sx, "y": sy, "button": "left", "clickCount": 1,
                })),
            )
            .await?;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let steps = steps.max(1);
        for i in 1..=steps {
            let ratio = i as f64 / steps as f64;
            self.page
                .send(
                    "Input.dispatchMouseEvent",
                    Some(json!({
                        "type": "mouseMoved",
                        "x": sx + (tx - sx) * ratio,
                        "y": sy + (ty - sy) * ratio,
                        "button": "left",
                    })),
                )
                .await?;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        self.page
            .send(
                "Input.dispatchMouseEvent",
                Some(json!({
                    "type": "mouseReleased", "x": tx, "y": ty, "button": "left", "clickCount": 1,
                })),
            )
            .await?;
        Ok(())
    }
}

/// Read a numeric coordinate field from a `getElementCoords` response. The
/// bridge always returns all four coordinates on success (a failure is already
/// lifted by `parse_bridge_response`), so a missing field means the response
/// shape itself is wrong — surface it rather than actuating at the origin.
fn coord(resp: &Value, key: &str) -> Result<f64> {
    resp.get(key).and_then(Value::as_f64).ok_or_else(|| {
        WebPilotError::Other {
            detail: format!("getElementCoords response missing numeric field `{key}`"),
        }
        .into()
    })
}
