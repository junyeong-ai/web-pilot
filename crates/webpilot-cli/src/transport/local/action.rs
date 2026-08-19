//! Page-mutating actions: dispatch, drag, navigation passthrough.
//!
//! All bridge-routed actions forward the typed `Action` directly to
//! `bridge.js`'s `executeAction` handler; CDP-level actions (navigate, drag,
//! history, reload) are handled inline against the page CDP connection.

use anyhow::Result;
use serde_json::{Value, json};
use webpilot::protocol::ResponseData;
use webpilot::types::Download;
use webpilot::{Action, WebPilotError};

use super::LocalTransport;

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

/// The `frame` object of a top-frame `Page.frameNavigated`, or `None` for any
/// other event (including a subframe navigation, which carries a `parentId`).
/// The single source of "did the *main* frame just navigate?" — a subframe that
/// reloads on its own must never settle a main-frame wait at the pre-navigation
/// document, so both the click-settle replay and the history-traversal wait
/// route their frame-event matching through here.
fn main_frame_navigated(ev: &Value) -> Option<&Value> {
    if ev.get("method").and_then(Value::as_str) != Some("Page.frameNavigated") {
        return None;
    }
    let frame = ev.pointer("/params/frame")?;
    frame.get("parentId").is_none().then_some(frame)
}

/// CDP modifier bitmask (Alt=1, Ctrl=2, Meta=4, Shift=8).
fn modifier_mask(m: &webpilot::action::Modifiers) -> u32 {
    (m.alt as u32) | ((m.ctrl as u32) << 1) | ((m.meta as u32) << 2) | ((m.shift as u32) << 3)
}

/// The DOM `code` and Windows virtual-key code for a key, so `Input.dispatchKeyEvent`
/// fires real native behaviour (Tab traversal, Backspace deletion, arrow nav)
/// that a synthetic `KeyboardEvent` cannot. Unknown keys carry no code (0).
fn key_descriptor(key: &str) -> Option<(String, u32)> {
    let named: Option<(&str, u32)> = match key {
        "Enter" => Some(("Enter", 13)),
        "Tab" => Some(("Tab", 9)),
        "Escape" => Some(("Escape", 27)),
        "Backspace" => Some(("Backspace", 8)),
        "Delete" => Some(("Delete", 46)),
        "ArrowUp" => Some(("ArrowUp", 38)),
        "ArrowDown" => Some(("ArrowDown", 40)),
        "ArrowLeft" => Some(("ArrowLeft", 37)),
        "ArrowRight" => Some(("ArrowRight", 39)),
        "Home" => Some(("Home", 36)),
        "End" => Some(("End", 35)),
        "PageUp" => Some(("PageUp", 33)),
        "PageDown" => Some(("PageDown", 34)),
        "Insert" => Some(("Insert", 45)),
        "CapsLock" => Some(("CapsLock", 20)),
        " " | "Space" => Some(("Space", 32)),
        _ => None,
    };
    if let Some((code, vk)) = named {
        return Some((code.to_string(), vk));
    }
    // F1–F12 only, in canonical form. A leading zero (`F01`) or extra digits
    // (`F007`) are not real DOM key codes; reject them rather than normalize to
    // F1, matching the browser regex `^F([1-9]|1[0-2])$` so a non-canonical name
    // fails identically in both modes instead of succeeding only in headless.
    if let Some(rest) = key.strip_prefix('F')
        && let Ok(n) = rest.parse::<u32>()
        && (1..=12).contains(&n)
        && rest == n.to_string()
    {
        return Some((format!("F{n}"), 111 + n));
    }
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() {
            let up = c.to_ascii_uppercase();
            return Some((format!("Key{up}"), up as u32));
        }
        if c.is_ascii_digit() {
            return Some((format!("Digit{c}"), c as u32));
        }
        // Any other single character carries no platform code/vk but types via
        // its `text`.
        return Some((String::new(), 0));
    }
    // A multi-character string that is neither a named key nor F1–F12 is not a
    // key — reject it rather than dispatch a no-op that reports success.
    None
}

/// The text a key contributes to its `keyDown`, or `None` for a key that
/// produces none. A single printable character and Space insert that
/// character; `Enter` carries a carriage return — the signal Chromium's
/// implicit form submission keys on, without which `key-press Enter` fires
/// listeners but never submits. Other named keys (Tab, arrows, Backspace)
/// produce no text: their effect is the keypress itself, and a stray "\t"
/// would type a tab instead of traversing focus.
/// A shifted ASCII letter is its uppercase form on every Latin layout, so a
/// `--shift` key-press of a letter produces the uppercase character. Shifted
/// digits/punctuation are layout-specific (US `1`→`!`), so they are left
/// unchanged rather than assume a keyboard layout.
fn shift_letter(s: &str, shift: bool) -> String {
    if shift && s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic() {
        s.to_ascii_uppercase()
    } else {
        s.to_owned()
    }
}

fn printable_key_text(key: &str) -> Option<String> {
    match key {
        "Enter" => Some("\r".to_string()),
        "Space" => Some(" ".to_string()),
        _ => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if !c.is_control() => Some(c.to_string()),
                _ => None,
            }
        }
    }
}

impl LocalTransport {
    // Policy is enforced once at the transport boundary (`Transport::send`),
    // so handlers run only after the command is permitted.
    pub(super) async fn do_action(
        &mut self,
        action: Action,
        capture: bool,
    ) -> Result<ResponseData> {
        // Subscribed before anything runs, for every action kind: a reload whose
        // page exports on load, or a drag that a handler turns into a file, is
        // no less a download than a click on a link, and classifying kinds as
        // unable to download is how one gets missed.
        let mut download_events = self.browser.subscribe_events();
        match &action {
            Action::Navigate { url } => {
                let url = url.clone();
                let downloads = self.navigate_reconnect(&url).await?;
                // Report where the navigation actually landed (after redirects).
                let landed = self.bound_target_url().await;
                let url_changed = (!landed.is_empty()).then_some(landed);
                return Ok(self
                    .settled_action_result(capture, url_changed, downloads)
                    .await);
            }
            Action::Back => {
                self.history_nav(HistoryNav::Back).await?;
                let downloads = self.downloads_from(&mut download_events, false).await;
                return Ok(self.settled_action_result(capture, None, downloads).await);
            }
            Action::Forward => {
                self.history_nav(HistoryNav::Forward).await?;
                let downloads = self.downloads_from(&mut download_events, false).await;
                return Ok(self.settled_action_result(capture, None, downloads).await);
            }
            Action::Reload => {
                // Subscribe BEFORE issuing the reload, so the completion event
                // can't fire in the gap before a fresh subscription and be lost —
                // the same race the click and history paths avoid by pre-subscribing.
                // Settle at the SAME point as navigate and browser-mode reload:
                // committed (the reload's main-frame `Page.frameNavigated`) then
                // parsed (`readyState` past `loading` — the DOMContentLoaded point a
                // capture acts on). `Page.loadEventFired` over-waited for trailing
                // subresources, so headless reload+capture lagged browser mode (which
                // settles at the parsed document) and blocked on slow images that add
                // no interactive elements. The commit gate matters because a reload
                // keeps the URL: probing `readyState` without first seeing the new
                // document commit could read the OLD document's `complete`. Best-effort
                // and deadline-bounded: a page that never commits/parses still settles
                // on whatever it reached at the deadline.
                let mut events = self.page.subscribe_events();
                let deadline =
                    tokio::time::Instant::now() + webpilot::settings::timeouts().reload_wait;
                self.page.send("Page.reload", None).await?;
                let committed = self
                    .page
                    .wait_on_receiver(
                        &mut events,
                        deadline.saturating_duration_since(tokio::time::Instant::now()),
                        |ev| main_frame_navigated(ev).is_some(),
                    )
                    .await;
                if committed {
                    while !self.document_parsed(deadline).await {
                        if tokio::time::Instant::now() >= deadline {
                            break;
                        }
                        tokio::time::sleep(webpilot::settings::timeouts().poll_interval).await;
                    }
                }
                self.clear_active_frame().await;
                let downloads = self.downloads_from(&mut download_events, false).await;
                return Ok(self.settled_action_result(capture, None, downloads).await);
            }
            Action::Drag {
                source,
                target,
                steps,
            } => {
                self.require_main_frame("drag").await?;
                self.do_drag(*source, *target, *steps).await?;
                let downloads = self.downloads_from(&mut download_events, false).await;
                return Ok(self.settled_action_result(capture, None, downloads).await);
            }
            Action::Hover { index } => {
                // Browser-input mouse move so CSS `:hover` actually fires.
                // Bridge.js dispatchEvent only triggers JS listeners, not the
                // internal hover state.
                self.require_main_frame("hover").await?;
                self.do_hover(*index).await?;
                let downloads = self.downloads_from(&mut download_events, false).await;
                return Ok(self.settled_action_result(capture, None, downloads).await);
            }
            Action::Upload { index, path } => {
                // No `require_main_frame`: upload resolves the index in the ACTIVE
                // frame's bridge world and sets the file on a frame-independent CDP
                // objectId (`DOM.setFileInputFiles`), with no viewport coordinate
                // or main-document lookup — so it works on a file input inside a
                // switched iframe, and gating it would be a constraint the
                // mechanism doesn't need.
                let path = path.clone();
                self.do_upload(*index, &path).await?;
                let downloads = self.downloads_from(&mut download_events, false).await;
                return Ok(self.settled_action_result(capture, None, downloads).await);
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
        // Snapshot the switched iframe's bridge context before the action: a click
        // that navigates that iframe replaces its document (a new execution
        // context), and a context id different from this one is how the settle
        // below knows to wait for the new page rather than capture the old one.
        let active_cid_before = if self.active_frame_id.lock().await.is_some() {
            self.bridge_context_id().await.ok()
        } else {
            None
        };
        let mut target_events = self.browser.subscribe_events();
        let mut page_events = self.page.subscribe_events();
        let mut frame_events = self.page.subscribe_events();

        // `key_press` dispatches a native CDP key event (real Tab/Backspace/
        // arrow/text behaviour), but still flows through the navigation +
        // popup detection below because Enter can submit a form. Every other
        // page-mutating action runs in the page via the bridge.
        let (nav_hint, frame_navigates, download_hint, opens_context, named_frame) = match &action {
            Action::KeyPress { key, modifiers } => {
                self.do_key_press(key, modifiers).await?;
                // Enter can submit a form, and that navigation is QUEUED (HTML
                // spec) — its start event may land after the key-dispatch response,
                // so the buffered drain alone can miss it and `--capture` would
                // snapshot the pre-submit page. Hint `nav` conservatively for Enter
                // so the settle waits (PROBE-bound) for the commit, exactly as a
                // link click's `navigates` hint does. A non-submitting Enter just
                // pays that short probe. Other keys never navigate.
                (key == "Enter", false, false, false, None)
            }
            _ => {
                let action_json = serde_json::to_value(&action)?;
                let raw = self
                    .invoke_bridge(&json!({"type": "executeAction", "action": action_json}))
                    .await?;
                let resp = Self::parse_bridge_response(raw)?;
                // `navigates`: a click that will load a new TOP document (drives
                // `url_changed` + the main settle). `frame_navigates`: the same for
                // the CURRENT frame — the only signal for an iframe-internal nav
                // under a switched frame, which the top URL never reflects.
                let navigates = resp
                    .get("navigates")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let frame_navigates = resp
                    .get("frame_navigates")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                // The Navigation API told the bridge this click began a download.
                // Chrome will announce it, so the drain below waits for it — a
                // download loads no document, so nothing else in the settle would.
                let downloads = resp
                    .get("downloads")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                // The click loads a document in a context this page cannot name.
                // It settles nothing here, but it decides whether a missing popup
                // below means "the tab became a download".
                let opens_context = resp
                    .get("opens_context")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                // A target NAME the clicking frame could not resolve. The frame
                // tree is the authority on browsing-context names, so a match
                // there means the click loads an EXISTING frame of this page
                // rather than opening a context.
                let named_frame = match resp.get("target_name").and_then(Value::as_str) {
                    Some(name) => self.frame_named(name).await,
                    None => None,
                };
                // A name reaches here only alongside the bridge's own
                // new-context verdict, so resolving it is the one thing that can
                // withdraw that verdict.
                let opens_context = opens_context && named_frame.is_none();
                (
                    navigates,
                    frame_navigates,
                    downloads,
                    opens_context,
                    named_frame,
                )
            }
        };

        // Sample the acted-on page before any pin move, so `url_changed`
        // reports this tab's navigation, never the popup's URL.
        let landed = self
            .settled_action_url(&mut page_events, &url_before, nav_hint)
            .await;
        let url_changed = (!landed.is_empty() && landed != url_before).then_some(landed);

        // The top URL alone conflates three distinct post-click outcomes; settle
        // each correctly rather than keying everything off `url_changed`:
        //   1. the switched iframe VANISHED — a main-frame nav destroyed it
        //      (whether or not the URL changed: a `target=_top` link to the same
        //      URL, a reload), or the page removed it. Drop the dead scope and
        //      settle the new main document.
        //   2. no switched frame and the MAIN URL changed — settle main.
        //   3. a switched iframe is still LIVE — an iframe-only navigation (the
        //      active-frame settle below), or a top-frame pushState that left the
        //      iframe intact. A same-document URL change is not a new document, so
        //      resetting on `url_changed` here would wrongly drop a live frame.
        let frame_vanished = !self.active_frame_still_present().await;
        if frame_vanished {
            self.clear_active_frame().await;
        }
        let has_active_frame = self.active_frame_id.lock().await.is_some();
        if frame_vanished || (!has_active_frame && url_changed.is_some()) {
            // A new MAIN document. The CDP page session survives a cross-site
            // renderer swap (the /devtools/page endpoint lives browser-side —
            // verified against a file://→http:// process swap), so no rebind: wait
            // for the fresh main world to name the live parsed document, since the
            // context can briefly still be the transitional pre-commit one and the
            // auto-capture below would read a page about to be replaced. (A
            // top-only pushState with no switched frame lands here; await_live is a
            // no-op when the document didn't actually change.)
            self.await_live_bridge_context().await;
        } else if has_active_frame && frame_navigates {
            // A click inside the switched iframe that navigated THAT iframe built a
            // new document in it without touching the top URL — invisible to the
            // main settle. Wait for the active frame's bridge context to name the
            // new live document, or the snapshot is the pre-click page.
            self.await_live_active_frame_context(active_cid_before)
                .await;
        }

        // Adoption moves the pin to the popup, so the page this command acted on
        // has to be identified before it runs. Only a click that opens a context
        // pays the frame-tree read; every other click keeps the lazy path, where
        // the tree is read at all only if something announced.
        let acted_on = self.target_id.clone();
        let acted_on_frames = if opens_context {
            Some(self.page_frame_ids().await)
        } else {
            None
        };

        // Adopt a click-opened tab BEFORE the capture: the pin moves to the
        // popup (the browser-mode contract), and the agent's snapshot must
        // describe the tab it will act on next — capturing the opener would
        // hand back indices that resolve nowhere.
        let new_tab = self.adopt_click_opened_target(&mut target_events).await;

        // A click that opens a new context and leaves no tab to adopt opened one
        // Chrome then discarded — which is exactly what it does when the response
        // turns out to be an attachment. So a download IS coming, and the drain
        // waits for it the way `isDownload` makes a navigation wait; a popup that
        // really is a page was adopted, and waits for nothing.
        // A click into a frame the settle does not cover — neither the main frame
        // nor the active one — ends nowhere this command otherwise looks. Wait
        // for that frame to commit: an ordinary navigation returns as soon as it
        // does, while a response that turns out to be an attachment never
        // commits at all and the announcement is what arrives instead.
        if let Some(frame_id) = named_frame {
            self.await_frame_navigation(&mut frame_events, &frame_id)
                .await;
        }

        let vanished_context = opens_context && new_tab.is_none();
        let sweep = super::collect_downloads(
            &mut download_events,
            &self.downloads_dir(),
            &acted_on,
            download_hint || vanished_context,
        )
        .await;
        let downloads = self.credit_downloads(sweep, acted_on_frames).await;

        // Capture AFTER everything settled: for a navigating click that is
        // the committed-and-parsed new document, for a popup the adopted tab.
        // A capture failure must not fail the command — the action's side
        // effect is done, and a retry would run it twice — so it is reported
        // alongside the success as `capture_error`.
        let (dom, capture_error) = if capture {
            // An adopted popup is already settled — `adopt_click_opened_target`
            // waited past about:blank to read its identity. Only a same-tab
            // navigation still needs a readiness wait before the snapshot.
            if new_tab.is_none() && url_changed.is_some() {
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
            downloads,
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
        // Scoped to the active frame inside `count_http_subframes` (correct from
        // the main frame and a switched one), so no main-frame gate here.
        snapshot.subframes = self.count_http_subframes().await;
        Ok(snapshot)
    }

    /// Build the result for an action that has already settled its own page
    /// change — navigation, history, reload, drag, hover, upload — honouring
    /// `--capture` with a post-settle snapshot exactly like the fall-through
    /// click/type path and browser mode (whose auto-capture runs after every
    /// action). A capture failure is reported as `capture_error` beside the
    /// success, never a command failure (a retry would re-run the side effect).
    async fn settled_action_result(
        &self,
        capture: bool,
        url_changed: Option<String>,
        downloads: Vec<Download>,
    ) -> ResponseData {
        let (dom, capture_error) = if capture {
            match self.capture_action_snapshot().await {
                Ok(snapshot) => (Some(snapshot), None),
                Err(e) => (None, Some(e.to_string())),
            }
        } else {
            (None, None)
        };
        ResponseData::Action {
            success: true,
            error: None,
            dom,
            url_changed,
            new_tab: None,
            capture_error,
            downloads,
        }
    }

    /// Typed guard for actions whose CDP path uses page-viewport coordinates
    /// (`drag`, `hover`) and so cannot target an iframe: inside a switched frame
    /// they would silently act on the wrong position — fail loudly instead.
    /// (Index-resolved actions like `click`/`upload` run in the active frame's
    /// own bridge world, so they are NOT gated here.)
    pub(super) async fn require_main_frame(&self, kind: &str) -> Result<()> {
        if self.active_frame_id.lock().await.is_some() {
            return Err(WebPilotError::InvalidArgument {
                detail: format!(
                    "'{kind}' targets the main frame only and an iframe is active. Switch back first: webpilot frame main"
                ),
            }
            .into());
        }
        Ok(())
    }

    async fn do_upload(&self, index: u32, path: &std::path::Path) -> Result<()> {
        // File inputs cannot be filled by page JS, so CDP must set the file by
        // node — and the node has to be the EXACT element the index addressed.
        // The bridge stashes that snapshot element by object identity (a stale
        // index is typed StaleSnapshot here, a non-file element a typed
        // InvalidArgument); we then resolve the stored reference to a CDP
        // objectId and set the file on THAT object. There is no marker attribute
        // and no document-order re-query, so a page can neither observe nor
        // redirect the target between resolve and sink, and the direct object
        // reaches a file input inside an open shadow root that a document-root
        // selector never could.
        let outcome = async {
            let prep = self
                .invoke_bridge(&json!({"type": "prepareUpload", "index": index}))
                .await?;
            Self::parse_bridge_response(prep)?;
            self.set_upload_target_file(index, path).await
        }
        .await;

        // Release the stored reference no matter how the attempt ended — even a
        // transport failure after `prepareUpload` stashed the element — so a
        // failed upload never pins a detached node in the bridge.
        let _ = self.invoke_bridge(&json!({"type": "clearUpload"})).await;
        outcome
    }

    /// Set `path` on the file input the bridge stashed as `state.uploadTarget`,
    /// resolved to a CDP objectId in the active context — but only while it is
    /// still in the DOM. A detached node keeps a live objectId, so the
    /// `isConnected` recheck (not a null check alone) is what turns a target the
    /// page removed between `prepareUpload` and here into a typed `StaleSnapshot`,
    /// never a silent file-set on an orphaned input.
    async fn set_upload_target_file(&self, index: u32, path: &std::path::Path) -> Result<()> {
        let object_id = self
            .eval_object_id(
                "(()=>{const t=window.__webpilot_state.uploadTarget;return t&&t.isConnected?t:null;})()",
            )
            .await?
            .ok_or(WebPilotError::StaleSnapshot { index })?;

        self.page
            .send(
                "DOM.setFileInputFiles",
                Some(json!({"objectId": object_id, "files": [path.to_string_lossy()]})),
            )
            .await?;
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
        events: &mut crate::cdp::SessionEvents,
        before: &str,
        nav_hint: bool,
    ) -> String {
        use tokio::sync::broadcast::error::{RecvError, TryRecvError};

        fn main_frame_url(ev: &Value) -> Option<&str> {
            main_frame_navigated(ev)?.get("url").and_then(Value::as_str)
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
        let mut lagged = false;
        loop {
            match events.try_recv() {
                Ok(ev) => {
                    if let Some(u) = main_frame_url(&ev) {
                        seq.push(Ev::Commit(u.to_string()));
                    } else if let Some(id) = ev.pointer("/params/frameId").and_then(Value::as_str) {
                        match ev.get("method").and_then(Value::as_str) {
                            Some("Page.frameStartedLoading") => {
                                seq.push(Ev::Started(id.to_string()))
                            }
                            Some("Page.frameStoppedLoading") => {
                                seq.push(Ev::Stopped(id.to_string()))
                            }
                            _ => {}
                        }
                    }
                }
                Err(TryRecvError::Lagged(_)) => {
                    lagged = true;
                    continue;
                }
                Err(_) => break,
            }
        }
        // An empty buffer usually means the action started no navigation, and
        // for most actions that is the whole truth — return now, pay nothing.
        // But a link click QUEUES its navigation (HTML spec), so its
        // `frameStartedLoading` can be emitted on a later task, AFTER the bridge
        // click response, and miss this one-shot drain — the buffered-event
        // assumption is optimistic, not guaranteed (CDP gives no ordering
        // barrier between a Page event and the click's Runtime response). The
        // bridge therefore reports `navigates` for a click it determined will
        // load a new top-level document; when it does, fall through to the
        // commit-wait rather than concluding "nothing happened". (A `location=`
        // from a later timer carries no hint and stays the agent's `wait`.)
        // The common no-navigation case returns before any extra round-trip.
        if seq.is_empty() && !nav_hint {
            if lagged {
                return self.bound_target_url().await;
            }
            return immediate;
        }
        // Both the buffered replay and the live wait below only settle on the
        // MAIN frame — a subframe's start/stop must never satisfy the wait, or a
        // cross-origin iframe that reloads on its own would end the wait at the
        // pre-click URL and DOM. Learn the main frame id once, for both.
        // (Unreadable tree → don't wait.)
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
        if !seq.is_empty() {
            // Replay IN ORDER (not set membership) so `[priorStop, ourStart]`
            // resolves to "still loading" instead of short-circuiting on the
            // stale stop.
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
                if lagged {
                    return self.bound_target_url().await;
                }
                return immediate;
            }
        }
        // Otherwise seq was empty but a navigation is expected (`nav_hint`): its
        // first event has not landed yet — wait for the commit below (PROBE-bound).

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
                    // The MAIN load ended without a commit (cancelled, download):
                    // nothing further to wait for. A subframe's stop is ignored —
                    // an unrelated iframe reloading must not end the main wait at
                    // the pre-click page.
                    if ev.get("method").and_then(Value::as_str) == Some("Page.frameStoppedLoading")
                        && ev.pointer("/params/frameId").and_then(Value::as_str)
                            == Some(main.as_str())
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
    async fn await_document_ready(&self, events: &mut crate::cdp::SessionEvents) {
        use tokio::sync::broadcast::error::RecvError;
        // The whole wait — both the readyState probes AND the event waits — is
        // bounded by one PROBE deadline, so a hung renderer can never stretch a
        // probe to the 30s CDP send timeout and blow the budget.
        let deadline = tokio::time::Instant::now() + super::PROBE;
        if self.document_parsed(deadline).await {
            return;
        }
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
                    if ev.get("method").and_then(Value::as_str) == Some("Page.domContentEventFired")
                        && self.document_parsed(deadline).await
                    {
                        return;
                    }
                }
                Ok(Err(RecvError::Lagged(_))) => continue,
                Ok(Err(_)) | Err(_) => return,
            }
        }
    }

    /// `document.readyState` is past `loading`, probed within the remaining
    /// `deadline` budget — so a stuck renderer's evaluate can't exceed the
    /// caller's wait window by falling back to the CDP send timeout.
    async fn document_parsed(&self, deadline: tokio::time::Instant) -> bool {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        matches!(
            tokio::time::timeout(deadline - now, self.page.evaluate("document.readyState")).await,
            Ok(Ok(v)) if v.as_str().is_some_and(|s| s != "loading")
        )
    }

    /// Wait — bounded by `PROBE` — for `frame_id` to commit a document.
    ///
    /// The receiver is subscribed before the action, so an event emitted while
    /// the click was in flight is already buffered and cannot be missed.
    async fn await_frame_navigation(&self, events: &mut crate::cdp::SessionEvents, frame_id: &str) {
        use tokio::sync::broadcast::error::{RecvError, TryRecvError};

        let committed = |event: &Value| {
            event.get("method").and_then(Value::as_str) == Some("Page.frameNavigated")
                && event.pointer("/params/frame/id").and_then(Value::as_str) == Some(frame_id)
        };
        let mut lagged = false;
        loop {
            match events.try_recv() {
                Ok(event) if committed(&event) => return,
                Ok(_) => continue,
                Err(TryRecvError::Lagged(_)) => {
                    lagged = true;
                    continue;
                }
                Err(_) => break,
            }
        }
        let deadline = tokio::time::Instant::now() + super::PROBE;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                if lagged {
                    tracing::debug!(
                        "CDP event backlog overflowed while waiting for a frame to commit; \
                         the wait ran to its bound instead of ending at the commit"
                    );
                }
                return;
            }
            match tokio::time::timeout(deadline - now, events.recv()).await {
                Ok(Ok(event)) if committed(&event) => return,
                Ok(Ok(_)) => continue,
                Ok(Err(RecvError::Lagged(_))) => {
                    lagged = true;
                    continue;
                }
                Ok(Err(RecvError::Closed)) | Err(_) => return,
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
    pub(super) async fn await_adopted_document(&self, events: &mut crate::cdp::SessionEvents) {
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

    /// Dispatch a key as a real browser input event via CDP, so native
    /// behaviour (Tab focus traversal, Backspace deletion, arrow navigation,
    /// printable text insertion, Enter submitting a form) actually fires —
    /// a synthetic `KeyboardEvent` only notifies JS listeners. Printable text
    /// is inserted only without a chord modifier (Ctrl/Alt/Meta make the key a
    /// shortcut, not input).
    async fn do_key_press(&self, key: &str, mods: &webpilot::action::Modifiers) -> Result<()> {
        let modifiers = modifier_mask(mods);
        let (code, vk) = key_descriptor(key).ok_or_else(|| WebPilotError::InvalidArgument {
            detail: format!(
                "Unknown key: {key:?} — use a single character, a named key \
                 (Enter/Tab/Escape/Backspace/Delete/Arrow*/Home/End/PageUp/PageDown/Space/Insert/CapsLock), \
                 or F1–F12"
            ),
        })?;
        let text = (!mods.ctrl && !mods.alt && !mods.meta)
            .then(|| printable_key_text(key))
            .flatten();
        // A shifted ASCII letter is its uppercase form on every Latin layout, so
        // honor it: `key-press a --shift` delivers "A" — both the inserted `text`
        // and the event `key` — not "a" with only the shiftKey flag (which leaves
        // a field lowercase and an `e.key === "A"` listener unmatched). Shifted
        // digits/punctuation are layout-specific (US `1`→`!`, others differ), so
        // those are left unchanged rather than assume a keyboard layout. `code`
        // and `vk` stay keyed off the unshifted key — they are layout-position,
        // not the produced character.
        // The spacebar's canonical DOM `key` is " " — the character it produces,
        // not the "Space" token a caller may spell it as. Chrome rejects "Space"
        // as a `key` value (it lands as an empty `e.key`), so a listener keying on
        // `e.key === " "` would miss the `Space` spelling; normalize to the same
        // character `printable_key_text` already yields. Every other named key
        // uses its canonical DOM key as its token, so only the spacebar needs this.
        let key = if key == "Space" { " " } else { key };
        let key = shift_letter(key, mods.shift);
        let text = text.map(|t| shift_letter(&t, mods.shift));

        // The bitmask alone does not PRESS the modifier: Chromium's renderer-level
        // editing commands (Shift+Arrow selection extension, etc.) key off real
        // modifier key events, so a chord is bracketed like a physical keyboard —
        // each held modifier goes down (rawKeyDown, accumulating the mask the way
        // real typing does) before the main key and comes up in reverse order
        // after it. Empirically verified: mask-only Shift+ArrowLeft left the
        // selection untouched; bracketed it extends the selection. (Browser-level
        // accelerators — select-all/copy/paste — are handled in the browser
        // process and are NOT reachable via injected key events; only
        // renderer-level editing is.) (Bits mirror `modifier_mask`: Alt=1 Ctrl=2
        // Meta=4 Shift=8.)
        let held: Vec<(&str, &str, u32, u32)> = [
            (mods.ctrl, ("Control", "ControlLeft", 17u32, 2u32)),
            (mods.alt, ("Alt", "AltLeft", 18, 1)),
            (mods.shift, ("Shift", "ShiftLeft", 16, 8)),
            (mods.meta, ("Meta", "MetaLeft", 91, 4)),
        ]
        .into_iter()
        .filter_map(|(on, m)| on.then_some(m))
        .collect();
        // A modifier that went down MUST come back up even when a later send
        // fails on a still-live connection (a transient timeout): a latched
        // Control would turn every subsequent click into a ctrl-click. So the
        // presses and the main key run first, recording what actually went
        // down; the releases then always run, in reverse, before any error is
        // propagated — the main error first, else the first release failure
        // (a stuck key reported as success would be the same lie).
        let mut pressed: Vec<&(&str, &str, u32, u32)> = Vec::new();
        let mut acc = 0u32;
        let main = async {
            for m in &held {
                let (mkey, mcode, mvk, bit) = m;
                acc |= bit;
                self.page
                    .send(
                        "Input.dispatchKeyEvent",
                        Some(json!({
                            "type": "rawKeyDown",
                            "modifiers": acc,
                            "key": mkey,
                            "code": mcode,
                            "windowsVirtualKeyCode": mvk,
                        })),
                    )
                    .await?;
                pressed.push(m);
            }

            // `nativeVirtualKeyCode` is deliberately omitted: it is the
            // platform-native scan code (different on macOS vs Windows), and
            // sending the Windows code on macOS makes Chrome mis-map the key to
            // an unrelated browser accelerator. `windowsVirtualKeyCode` +
            // `key` + `code` is the portable set Chrome resolves from on every
            // platform.
            let mut down = json!({
                "type": "keyDown",
                "modifiers": modifiers,
                "key": key,
                "code": code,
                "windowsVirtualKeyCode": vk,
            });
            if let Some(t) = &text {
                down["text"] = json!(t);
            }
            self.page.send("Input.dispatchKeyEvent", Some(down)).await?;
            self.page
                .send(
                    "Input.dispatchKeyEvent",
                    Some(json!({
                        "type": "keyUp",
                        "modifiers": modifiers,
                        "key": key,
                        "code": code,
                        "windowsVirtualKeyCode": vk,
                    })),
                )
                .await
        }
        .await;

        // Release every modifier that actually went down — regardless of how
        // the chord fared. One failed release still tries the rest (maximal
        // cleanup); the first release error is kept so a stuck key is never
        // silent.
        let mut acc = pressed.iter().fold(0u32, |m, (_, _, _, bit)| m | bit);
        let mut release_err: Option<anyhow::Error> = None;
        for (mkey, mcode, mvk, bit) in pressed.iter().rev() {
            acc &= !bit;
            if let Err(e) = self
                .page
                .send(
                    "Input.dispatchKeyEvent",
                    Some(json!({
                        "type": "keyUp",
                        "modifiers": acc,
                        "key": mkey,
                        "code": mcode,
                        "windowsVirtualKeyCode": mvk,
                    })),
                )
                .await
            {
                release_err.get_or_insert(e);
            }
        }
        main?;
        release_err.map_or(Ok(()), Err)
    }

    /// Drive a browser history traversal (`history.back()`/`forward()`).
    ///
    /// Decided by OUTCOME, never prediction. `navigation.canGoBack/Forward` only
    /// sees the contiguous **same-origin** run of session history (Navigation
    /// API spec), so it returns `false` for a cross-origin adjacent entry that
    /// `history.back()` traverses to fine — using it as a guard would falsely
    /// report "no history entry" across every origin boundary (an OAuth/SSO
    /// redirect, leaving a search engine for a result), blocking a valid
    /// traversal. Instead we issue the traversal and settle on what actually
    /// happened: a real traversal fires a main-frame navigation — a new document
    /// (`Page.frameNavigated`) or a same-document/bfcache hop
    /// (`Page.navigatedWithinDocument`) — and returns at once; a genuine no-op
    /// (already at the first/last entry) fires nothing and surfaces as a typed
    /// `NavigationFailed` when the window closes.
    ///
    /// The traversal tears down the execution context the expression runs in, so
    /// the evaluate's own response can come back as a CDP "target navigated"
    /// teardown — that is the success path, not a failure. Only typed transport
    /// failures (`ConnectionLost`/`Timeout`) are propagated.
    async fn history_nav(&self, direction: HistoryNav) -> Result<()> {
        let expression = match direction {
            HistoryNav::Back => "history.back()",
            HistoryNav::Forward => "history.forward()",
        };
        let before = self.bound_target_url().await;
        // The history position BEFORE the traversal — compared to the position
        // after the wait to tell a real hop (the index moved) from a genuine
        // no-op, even when the navigation event was dropped under event-ring lag
        // or the URL never changed (a same-URL / same-document entry). See the
        // settle fallback below.
        let before_index = self.nav_history_index().await;
        // Subscribe BEFORE issuing the traversal so the outcome events are
        // buffered for both the traversal check and `await_document_ready` — no
        // event can fire between the evaluate and a later subscribe and be lost,
        // the race a fresh `wait_for_event_matching` subscription would open.
        let mut events = self.page.subscribe_events();
        if let Err(e) = self.page.evaluate(expression).await
            && matches!(
                e.downcast_ref::<WebPilotError>(),
                Some(WebPilotError::ConnectionLost { .. } | WebPilotError::Timeout { .. })
            )
        {
            return Err(e);
        }
        // Settle only on the MAIN frame: a subframe that reloads during the
        // window must never satisfy the wait, or a cross-origin iframe reloading
        // on its own would end it at the pre-traversal document (the click-settle
        // path and browser mode filter the same way). A new document is
        // `Page.frameNavigated` (no parentId); a same-document/bfcache hop is
        // `Page.navigatedWithinDocument` for the main frame's id.
        let main_id = self.main_frame_id.clone();
        let mut traversed = self
            .page
            .wait_on_receiver(
                &mut events,
                webpilot::settings::timeouts().back_forward,
                |ev| {
                    main_frame_navigated(ev).is_some()
                        || (ev.get("method").and_then(Value::as_str)
                            == Some("Page.navigatedWithinDocument")
                            && ev.pointer("/params/frameId").and_then(Value::as_str)
                                == Some(main_id.as_str()))
                },
            )
            .await;
        // The awaited event can be lost two ways before the deadline: a busy
        // page's event burst can overflow the broadcast ring and DROP it
        // (`wait_on_receiver` can't tell that from genuine absence), or a
        // same-URL/same-document hop simply never moves the URL. Before declaring
        // "no history entry", confirm the negative against the navigation
        // history's CURRENT INDEX — the definitive position signal, which moves
        // iff a real traversal landed (whatever the hop's document or URL) and
        // survives a dropped event. Only if the index is unreadable in either
        // sample do we fall back to the weaker "the URL moved" heuristic.
        if !traversed {
            traversed = match (before_index, self.nav_history_index().await) {
                (Some(b), Some(a)) => a != b,
                _ => {
                    let after = self.bound_target_url().await;
                    !after.is_empty() && after != before
                }
            };
        }
        if !traversed {
            return Err(WebPilotError::NavigationFailed {
                url: expression.to_string(),
                reason: "no history entry".into(),
            }
            .into());
        }
        // Wait — best-effort — for the traversed-to document to parse, so a
        // following capture reads a ready page, not a committed-but-empty one
        // (browser mode waits the same readyState bar). The immediate readyState
        // check makes a same-document/bfcache traversal return without delay.
        self.await_document_ready(&mut events).await;
        self.clear_active_frame().await;
        Ok(())
    }

    /// The current entry's index in the page's navigation history, or `None` if
    /// it can't be read. The definitive position signal for a history traversal:
    /// it moves iff a real back/forward landed, regardless of the document's URL
    /// — so it catches a same-URL / same-document hop (a `pushState` entry whose
    /// URL is unchanged) that a URL compare misses, and it survives the
    /// navigation event being evicted from the broadcast ring under a busy page's
    /// event burst. Best-effort: an unreadable history is `None`, and the caller
    /// falls back to the weaker URL-moved check.
    async fn nav_history_index(&self) -> Option<i64> {
        self.page
            .send("Page.getNavigationHistory", None)
            .await
            .ok()?
            .get("currentIndex")
            .and_then(Value::as_i64)
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

        // `buttons` (the held-button bitmask, 1 = left) must accompany every
        // event of the gesture: CDP tracks the drag through it, and a move
        // carrying `buttons: 0` resets that state so the final release is
        // treated as releasing a button that isn't down — silently ignored,
        // leaving the page mid-drag (mouseup never fires) while the command
        // reports success. Empirically verified: without it the page sees
        // mousedown + a single move and NO mouseup.
        self.page
            .send(
                "Input.dispatchMouseEvent",
                Some(json!({
                    "type": "mousePressed", "x": sx, "y": sy,
                    "button": "left", "buttons": 1, "clickCount": 1,
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
                        "buttons": 1,
                    })),
                )
                .await?;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        self.page
            .send(
                "Input.dispatchMouseEvent",
                Some(json!({
                    "type": "mouseReleased", "x": tx, "y": ty,
                    "button": "left", "buttons": 0, "clickCount": 1,
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

#[cfg(test)]
mod key_descriptor_tests {
    use super::key_descriptor;

    #[test]
    fn f_keys_accept_only_canonical_names() {
        // Canonical F1–F12 map to their DOM `code` + Windows VK (111 + n).
        assert_eq!(key_descriptor("F1"), Some(("F1".to_string(), 112)));
        assert_eq!(key_descriptor("F9"), Some(("F9".to_string(), 120)));
        assert_eq!(key_descriptor("F12"), Some(("F12".to_string(), 123)));
        // A leading zero or extra digits are not real DOM key codes — rejected,
        // not silently normalized to F1, so a non-canonical name fails the same
        // way the browser regex `^F([1-9]|1[0-2])$` makes it fail in browser mode.
        assert_eq!(key_descriptor("F01"), None);
        assert_eq!(key_descriptor("F007"), None);
        assert_eq!(key_descriptor("F0"), None);
        assert_eq!(key_descriptor("F13"), None);
    }
}
