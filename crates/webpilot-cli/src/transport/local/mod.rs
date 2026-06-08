//! Headless transport — speaks CDP directly to a Chrome for Testing instance.
//!
//! `LocalTransport` is the in-process equivalent of the Native Messaging Host
//! plus the extension service worker plus the content bridge. It owns the
//! browser-level and page-level CDP connections, plus the cached target id and
//! optional browser-context id needed for multi-agent isolation.
//!
//! Bridge.js auto-loads into the `webpilot_bridge` CDP isolated world on every
//! document (`install_bridge_world`) — the headless mirror of the browser
//! content script. Per-frame routing splits in two: `bridge_contexts` (the
//! isolated world, for `__webpilot_handle` calls) and `frame_contexts` (the
//! MAIN world, for page expressions and monitors). Both are set up in `open`
//! and rebound on page swaps (navigation, tab switch).
//!
//! The `do_*` command handlers are split across sibling modules by domain:
//!   - `action`  — page-mutating actions (click/type/scroll/drag/navigate/...)
//!   - `capture` — DOM extraction, screenshot, PDF, accessibility tree
//!   - `query`   — eval, wait, dom get/set, fetch
//!   - `state`   — cookies, console + network monitoring, session
//!   - `browser` — tab list/switch/new/close, frame list/switch, status

mod action;
mod browser;
mod capture;
mod query;
mod state;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::Mutex;

use webpilot::dirs;
use webpilot::protocol::{Command, ResponseData};
use webpilot::types::DomSnapshot;
use webpilot::{WebPilotError, WireError};

use crate::cdp::CdpClient;
use crate::session;

use super::Transport;
use super::local_context;

const BRIDGE_JS: &str = include_str!("../../../../../extension/content/bridge.js");

/// Name of the CDP isolated world the bridge runs in — the headless mirror of
/// the browser-mode content-script world. DOM-manipulating bridge calls execute
/// here, off the page's reach, so a hostile page cannot tamper with how an index
/// resolves; page expressions (`eval`, `frame find`) and the console/network
/// monitors stay in the MAIN world, where they belong.
const BRIDGE_WORLD: &str = "webpilot_bridge";

pub struct LocalTransport {
    pub(crate) browser: CdpClient,
    pub(crate) page: CdpClient,
    pub(crate) ws_url: String,
    pub(crate) browser_context_id: Option<String>,
    pub(crate) target_id: String,
    /// Top frame id (CDP string). Refreshed on every page swap (`rebind_page_world`),
    /// so it always names the bound tab's main frame. Resolves the bridge context
    /// for the main frame when no iframe is switched.
    pub(crate) main_frame_id: String,
    /// frame_id (CDP string) → MAIN-world **`uniqueContextId`**, for page
    /// expressions. Populated by the background subscriber from each frame's
    /// default (`isDefault`) `Runtime.executionContextCreated`. The unique id
    /// (not the reusable integer `id`) is stored so a stale entry can never
    /// resolve to a different context after a cross-process navigation.
    pub(crate) frame_contexts: Arc<Mutex<HashMap<String, String>>>,
    /// frame_id (CDP string) → the bridge isolated world's `uniqueContextId`.
    /// Populated by the same subscriber from the `BRIDGE_WORLD`-named context
    /// that `Page.addScriptToEvaluateOnNewDocument` creates per document.
    pub(crate) bridge_contexts: Arc<Mutex<HashMap<String, String>>>,
    /// Active frame for evaluation. `None` means the page's main world.
    pub(crate) active_frame_id: Arc<Mutex<Option<String>>>,
    /// Whether the console / network monitors are armed. Their hooks live on
    /// `window` and are wiped by every full-document navigation, so when armed
    /// they are re-installed after each navigation settles — matching the
    /// browser-mode service worker, which re-injects on `webNavigation`.
    pub(crate) console_monitoring: Arc<AtomicBool>,
    pub(crate) network_monitoring: Arc<AtomicBool>,
}

impl LocalTransport {
    /// Connect to a headless Chrome (launching one if needed) and resolve a
    /// page target. When `context_name` is `Some`, attaches to that context's
    /// page (creating the context on first call).
    pub async fn open(context_name: Option<&str>) -> Result<Self> {
        let ws_url = session::ensure_session().await?;
        // Chrome can exit between `ensure_session`'s liveness check and this
        // connect, leaving a stale URL. A connect failure there means the
        // session is dead, not that the command is impossible — discard it and
        // relaunch once before giving up, so a transient teardown doesn't
        // surface as a hard failure.
        let browser = match CdpClient::connect(&ws_url).await {
            Ok(browser) => browser,
            Err(_) => {
                // Invalidate only if this is still the session we failed on —
                // a concurrent `open` may have already relaunched a fresh one.
                session::invalidate_session_if_current(&ws_url);
                let ws_url = session::ensure_session().await?;
                CdpClient::connect(&ws_url).await?
            }
        };

        // Push-based target discovery: a click-opened popup is correlated by
        // the `Target.targetCreated` event captured during an action's bridge
        // window — the headless mirror of browser mode's `tabs.onCreated`
        // listener. Idempotent per connection.
        browser
            .send(
                "Target.setDiscoverTargets",
                Some(serde_json::json!({"discover": true})),
            )
            .await?;

        let (page, browser_context_id, target_id) =
            resolve_target(&browser, &ws_url, context_name).await?;

        let main_frame_id = fetch_main_frame_id(&page).await?;
        let frame_contexts = Arc::new(Mutex::new(HashMap::new()));
        let bridge_contexts = Arc::new(Mutex::new(HashMap::new()));
        spawn_frame_context_listener(&page, frame_contexts.clone(), bridge_contexts.clone());
        // Register the bridge so it auto-loads into its isolated world on every
        // document (current one included) — the headless equivalent of the
        // browser content script, with no per-call injection. `connect_to_page`
        // already enabled Runtime, but its initial `executionContextCreated`
        // events (and the bridge world's) predate the listener's subscription,
        // so toggle the domain to force re-emission for every existing context.
        install_bridge_world(&page).await?;
        let _ = page.send("Runtime.disable", None).await;
        let _ = page.send("Runtime.enable", None).await;

        // Re-apply device emulation across CLI invocations: a UA override does
        // not survive the prior process's CDP disconnect, so without this the
        // `device set --user-agent` an agent issued would be silently gone.
        if let Some(dev) = read_persisted_device(browser_context_id.as_deref()) {
            let _ = dev.apply(&page).await;
        }

        // Restore the active frame across CLI invocations. CLI calls are
        // separate processes, so without persistence `frame switch` would
        // be a no-op — the next `eval` would lose the active frame and
        // silently fall back to the main world.
        let persisted = read_persisted_active_frame(browser_context_id.as_deref());
        let restored_active = match persisted {
            Some(fid) if frame_exists(&page, &fid).await => Some(fid),
            Some(_) => {
                clear_persisted_active_frame(browser_context_id.as_deref());
                None
            }
            None => None,
        };

        // Restore the armed-monitor state across CLI invocations: `network
        // start` in one process must keep recording through navigations run
        // from later processes, exactly as the browser-mode service worker's
        // per-tab monitoring survives because the worker itself persists.
        let (console_armed, network_armed) = read_persisted_monitors(browser_context_id.as_deref());

        Ok(Self {
            browser,
            page,
            ws_url,
            browser_context_id,
            target_id,
            main_frame_id,
            frame_contexts,
            bridge_contexts,
            active_frame_id: Arc::new(Mutex::new(restored_active)),
            console_monitoring: Arc::new(AtomicBool::new(console_armed)),
            network_monitoring: Arc::new(AtomicBool::new(network_armed)),
        })
    }

    pub(crate) fn persisted_context_key(&self) -> Option<&str> {
        self.browser_context_id.as_deref()
    }

    pub fn page(&self) -> &CdpClient {
        &self.page
    }

    pub fn browser(&self) -> &CdpClient {
        &self.browser
    }

    // ── Frame routing ────────────────────────────────────────────────────

    /// The MAIN-world execution-context id `expression`s evaluate in: `None`
    /// means the page's default world (no iframe switched). A switched frame
    /// whose context has not announced itself within the probe window — the
    /// listener repopulates the map asynchronously after open/rebind — is a
    /// typed `FrameNotFound`, never a silent fall-through to the main world:
    /// that would run the agent's code in a frame it did not choose.
    async fn active_context_id(&self) -> Result<Option<String>> {
        let active = self.active_frame_id.lock().await.clone();
        let Some(fid) = active else { return Ok(None) };
        Ok(Some(self.await_context(&self.frame_contexts, &fid).await?))
    }

    /// The bridge isolated-world context for the active frame (the switched
    /// iframe, or the main frame when none is switched). Always explicit — the
    /// bridge never runs in the page's default world — so it polls for the
    /// `BRIDGE_WORLD` context the auto-injected script creates, surfacing a
    /// typed `FrameNotFound` if it never appears rather than silently routing a
    /// bridge call into the page's main world.
    async fn bridge_context_id(&self) -> Result<String> {
        let active = self.active_frame_id.lock().await.clone();
        let fid = active.unwrap_or_else(|| self.main_frame_id.clone());
        self.await_context(&self.bridge_contexts, &fid).await
    }

    /// Poll `map` for frame `fid`'s `uniqueContextId` until it appears or `PROBE`
    /// elapses — the contexts populate asynchronously as the listener observes
    /// `executionContextCreated`, so a just-navigated frame may not be mapped
    /// the instant a command fires.
    async fn await_context(
        &self,
        map: &Arc<Mutex<HashMap<String, String>>>,
        fid: &str,
    ) -> Result<String> {
        let deadline = std::time::Instant::now() + PROBE;
        loop {
            if let Some(cid) = map.lock().await.get(fid).cloned() {
                return Ok(cid);
            }
            if std::time::Instant::now() >= deadline {
                return Err(WebPilotError::FrameNotFound {
                    selector: format!("frame {fid}"),
                }
                .into());
            }
            tokio::time::sleep(webpilot::settings::timeouts().poll_interval).await;
        }
    }

    /// Evaluate `expression` in `context` (a `uniqueContextId`; the default
    /// world when `None`), returning either its serialized value or — when
    /// `by_value` is false — the `objectId` of the remote result under
    /// `/result/objectId`. `uniqueContextId` (not the reusable integer id) means
    /// a context destroyed between map read and send fails cleanly instead of
    /// landing in a different context that reused the integer.
    pub(super) async fn eval_in_context(
        &self,
        expression: &str,
        context: Option<&str>,
        by_value: bool,
    ) -> Result<Value> {
        self.eval_in_context_with_timeout(
            expression,
            context,
            by_value,
            webpilot::settings::timeouts().cdp_send,
        )
        .await
    }

    /// As `eval_in_context`, but bounds the CDP round-trip by an explicit
    /// deadline. A `wait` evaluates a polling loop *inside* the page that
    /// resolves only once its own `timeout_ms` elapses, so the CDP send must
    /// outlive that loop — under the default `cdp_send` a `wait --timeout 60`
    /// would otherwise be cut to a false 30s `Timeout` while the page is still
    /// legitimately waiting (the browser-mode twin runs on the extension's own
    /// timer and waits the full duration; this keeps the two in parity).
    pub(super) async fn eval_in_context_with_timeout(
        &self,
        expression: &str,
        context: Option<&str>,
        by_value: bool,
        timeout: std::time::Duration,
    ) -> Result<Value> {
        let mut params = json!({
            "expression": expression,
            "returnByValue": by_value,
            "awaitPromise": true,
        });
        if let Some(cid) = context {
            params["uniqueContextId"] = cid.into();
        }
        let result = self
            .page
            .send_with_timeout("Runtime.evaluate", Some(params), timeout)
            .await?;
        if let Some(exception) = result.get("exceptionDetails") {
            let msg = exception
                .pointer("/exception/description")
                .or_else(|| exception.pointer("/text"))
                .and_then(|v| v.as_str())
                .unwrap_or("JS exception");
            anyhow::bail!("{msg}");
        }
        Ok(result.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Evaluate a page expression in the active MAIN-world context (`eval`,
    /// `frame find`, readyState/title probes).
    pub(super) async fn eval_in_active(&self, expression: &str) -> Result<Value> {
        let cid = self.active_context_id().await?;
        let result = self
            .eval_in_context(expression, cid.as_deref(), true)
            .await?;
        Ok(result.get("value").cloned().unwrap_or(Value::Null))
    }

    /// Resolve a bridge-owned reference (`window.__webpilot_state.…`) to a CDP
    /// `objectId` — a live handle CDP can act on directly (e.g.
    /// `DOM.setFileInputFiles`). Runs in the bridge isolated world, where that
    /// state lives. `None` when the result is not an object (a primitive,
    /// `null`, or `undefined`), which the caller maps to its own typed error;
    /// keeping the object by reference (`returnByValue: false`) means identity,
    /// not a serialized copy, crosses to CDP.
    pub(super) async fn eval_object_id(&self, expression: &str) -> Result<Option<String>> {
        let cid = self.bridge_context_id().await?;
        let result = self.eval_in_context(expression, Some(&cid), false).await?;
        Ok(result
            .get("objectId")
            .and_then(|v| v.as_str())
            .map(String::from))
    }

    // ── Bridge invocation ────────────────────────────────────────────────

    /// Call `__webpilot_handle(msg)` in the bridge isolated world. The bridge
    /// auto-loads there on every document (`install_bridge_world`), so there is
    /// no per-call injection — only the context to target.
    pub(super) async fn invoke_bridge(&self, msg: &Value) -> Result<Value> {
        self.invoke_bridge_with_timeout(msg, webpilot::settings::timeouts().cdp_send)
            .await
    }

    /// As `invoke_bridge`, but bounds the CDP round-trip by `timeout` instead of
    /// the default `cdp_send`. Used by the `wait` path, whose in-page poll loop
    /// resolves only at its own deadline.
    pub(super) async fn invoke_bridge_with_timeout(
        &self,
        msg: &Value,
        timeout: std::time::Duration,
    ) -> Result<Value> {
        let payload = msg.to_string();
        let js = format!("(async () => __webpilot_handle({payload}))()");

        let cid = self.bridge_context_id().await?;
        match self
            .eval_in_context_with_timeout(&js, Some(&cid), true, timeout)
            .await
        {
            Ok(result) => Ok(result.get("value").cloned().unwrap_or(Value::Null)),
            Err(e) if is_stale_context(&e) => {
                // A navigation destroyed the isolated world this `cid` named, but
                // the context map can still hand it back until the async
                // `executionContextDestroyed` event is processed (a window the
                // slower the machine, the wider). Drop just that stale id and
                // re-resolve — `bridge_context_id` then waits for the new
                // document's context — and retry once. This is the renderer-swap
                // race Puppeteer/Playwright also retry through.
                self.bridge_contexts.lock().await.retain(|_, v| *v != cid);
                let fresh = self.bridge_context_id().await?;
                let result = self
                    .eval_in_context_with_timeout(&js, Some(&fresh), true, timeout)
                    .await?;
                Ok(result.get("value").cloned().unwrap_or(Value::Null))
            }
            Err(e) => Err(e),
        }
    }

    pub(super) fn parse_bridge_response(val: Value) -> Result<Value> {
        if val.get("success").and_then(|v| v.as_bool()) == Some(false)
            && let Some(error) = val.get("error")
        {
            let wire: WireError =
                serde_json::from_value(error.clone()).unwrap_or_else(|_| WireError {
                    code: "Other".into(),
                    message: error.to_string(),
                    data: Default::default(),
                });
            return Err(WebPilotError::from_wire(wire).into());
        }
        Ok(val)
    }

    // ── Navigation (with cross-origin renderer swap handling) ────────────

    /// Re-prime the bound page session: re-subscribe the context listener,
    /// re-register the bridge world, refresh the main frame id (a tab switch
    /// binds a different tab whose top frame differs), then re-emit existing
    /// contexts so both maps repopulate — the same priming `open` does, for the
    /// new session. The bridge install must succeed; a session that silently ran
    /// without it would answer every bridge call with `FrameNotFound`.
    pub(super) async fn rebind_page_world(&mut self) -> Result<()> {
        self.frame_contexts = Arc::new(Mutex::new(HashMap::new()));
        self.bridge_contexts = Arc::new(Mutex::new(HashMap::new()));
        spawn_frame_context_listener(
            &self.page,
            self.frame_contexts.clone(),
            self.bridge_contexts.clone(),
        );
        install_bridge_world(&self.page).await?;
        self.main_frame_id = fetch_main_frame_id(&self.page).await?;
        let _ = self.page.send("Runtime.disable", None).await;
        let _ = self.page.send("Runtime.enable", None).await;
        Ok(())
    }

    pub(super) async fn navigate_reconnect(&mut self, url: &str) -> Result<()> {
        let before_url = self.bound_target_url().await;

        // `Page.navigate` outcomes:
        // - `errorText` with a concrete net code (DNS, refused, CSP) → fail now.
        // - `errorText == net::ERR_ABORTED` → a cross-site swap superseded the old
        //   renderer's load; usually benign and the new load settles below, but
        //   keep it as the error to report if nothing ever settles.
        // - `Ok` with a `loaderId` → a document load; identifies this navigation
        //   for same-URL reloads, where the URL alone can't.
        // - `Ok` without a `loaderId` → a same-document (fragment) navigation;
        //   already complete, nothing to wait for.
        // - send `Err` → the page socket dropped mid swap; report it if the
        //   navigation never lands.
        let mut start_error = None;
        let loader_id = match self
            .page
            .send_with_timeout(
                "Page.navigate",
                Some(json!({"url": url})),
                std::time::Duration::from_secs(3),
            )
            .await
        {
            Ok(result) => {
                match result.get("errorText").and_then(|v| v.as_str()) {
                    Some("net::ERR_ABORTED") => {
                        start_error = Some(WebPilotError::NavigationFailed {
                            url: url.to_string(),
                            reason: "net::ERR_ABORTED".into(),
                        });
                    }
                    Some(reason) if !reason.is_empty() => {
                        return Err(WebPilotError::NavigationFailed {
                            url: url.to_string(),
                            reason: reason.to_string(),
                        }
                        .into());
                    }
                    _ => {}
                }
                let loader = result
                    .get("loaderId")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                // A same-document navigation (no new loader, no error) — a hash
                // or history change — is already complete and leaves the document
                // and its frames intact, so the active frame stays valid.
                if loader.is_none() && start_error.is_none() {
                    return Ok(());
                }
                loader
            }
            Err(e) => {
                start_error = Some(crate::into_webpilot_error(e));
                None
            }
        };

        let deadline = std::time::Instant::now() + webpilot::settings::timeouts().navigation;
        let loader = loader_id.as_deref();
        loop {
            if let Some((target_id, target_url)) = self.bound_target().await {
                if target_url != before_url {
                    // Cross-site or cross-page navigation: the renderer process
                    // may have swapped, leaving the old session's execution
                    // context dead. Rebind a fresh session to the new target and
                    // wait for it to commit and parse. A mid-swap target accepts
                    // the socket but isn't ready, so a failed connect just retries.
                    if let Ok(new_page) = connect_to_page(&self.ws_url, &target_id).await
                        && wait_navigation_settled(&new_page, loader, &before_url, deadline).await
                    {
                        self.page = new_page;
                        self.target_id = target_id;
                        self.clear_active_frame().await;
                        self.rebind_page_world().await?;
                        self.reinstall_monitors().await;
                        return Ok(());
                    }
                } else if navigation_settled(&self.page, loader, &before_url).await {
                    // Same-URL navigation: necessarily same-site, so the existing
                    // session stays valid (no renderer swap). The loader match
                    // distinguishes the reloaded document from the previous one.
                    self.clear_active_frame().await;
                    self.reinstall_monitors().await;
                    return Ok(());
                }
            }

            if std::time::Instant::now() >= deadline {
                return Err(start_error
                    .unwrap_or(WebPilotError::Timeout {
                        kind: "navigation".into(),
                        elapsed_ms: webpilot::settings::timeouts().navigation.as_millis() as u64,
                    })
                    .into());
            }
            tokio::time::sleep(webpilot::settings::timeouts().poll_interval).await;
        }
    }

    /// Drop the active frame after a navigation, in memory and on disk, so the
    /// next CLI invocation doesn't restore an iframe context the page has left.
    async fn clear_active_frame(&self) {
        *self.active_frame_id.lock().await = None;
        clear_persisted_active_frame(self.persisted_context_key());
    }

    /// `(target_id, url)` of the page WebPilot is bound to — the target it just
    /// navigated, identified by `self.target_id`. Falls back to the context's
    /// active page only if that target is gone (e.g. a swap replaced it), so a
    /// sibling tab in the same context is never mistaken for this navigation.
    async fn bound_target(&self) -> Option<(String, String)> {
        let targets = self.browser.get_targets().await.ok()?;
        let pick = targets
            .iter()
            .find(|t| t.get("targetId").and_then(|v| v.as_str()) == Some(&self.target_id))
            .or_else(|| find_page_in_context(&targets, self.browser_context_id.as_deref()))?;
        Some((
            pick.get("targetId").and_then(|v| v.as_str())?.to_string(),
            pick.get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        ))
    }

    /// URL of the bound page target (empty if it can't be read).
    pub(super) async fn bound_target_url(&self) -> String {
        self.bound_target()
            .await
            .map(|(_, url)| url)
            .unwrap_or_default()
    }
}

/// Whether an error is the typed [`crate::cdp::ContextGone`] — the renderer
/// swapped the document out from under a `uniqueContextId` the caller still
/// held. Matched by type (the CDP layer interprets the raw protocol error once),
/// never by re-parsing an error string here.
fn is_stale_context(err: &anyhow::Error) -> bool {
    err.downcast_ref::<crate::cdp::ContextGone>().is_some()
}

/// Per-probe time box: a context busy with an in-flight renderer swap can stall
/// an evaluate, so each readiness check is bounded and the caller's deadline
/// governs the overall wait.
const PROBE: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether the navigation WebPilot issued has committed on `page` and the
/// document has parsed. Committed = the main frame carries our `loader_id` (set
/// for same-URL reloads, where the URL never moves) OR its URL has left
/// `before_url` (a cross-page navigation, where the loader may differ across a
/// process swap). Parsed = `readyState` is past `loading` — DOMContentLoaded has
/// fired — so the page is usable without waiting on slow trailing subresources.
async fn navigation_settled(page: &CdpClient, loader_id: Option<&str>, before_url: &str) -> bool {
    let Ok(Ok(tree)) = tokio::time::timeout(PROBE, page.send("Page.getFrameTree", None)).await
    else {
        return false;
    };
    let frame = tree.pointer("/frameTree/frame");
    let loader = frame
        .and_then(|f| f.get("loaderId"))
        .and_then(|v| v.as_str());
    let frame_url = frame
        .and_then(|f| f.get("url"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let committed = (loader_id.is_some() && loader == loader_id) || frame_url != before_url;
    if !committed {
        return false;
    }
    matches!(
        tokio::time::timeout(PROBE, page.evaluate("document.readyState")).await,
        Ok(Ok(state)) if matches!(state.as_str(), Some("interactive") | Some("complete"))
    )
}

/// Poll a freshly-bound page until the navigation settles, bounded by `deadline`.
async fn wait_navigation_settled(
    page: &CdpClient,
    loader_id: Option<&str>,
    before_url: &str,
    deadline: std::time::Instant,
) -> bool {
    loop {
        if navigation_settled(page, loader_id, before_url).await {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(webpilot::settings::timeouts().poll_interval).await;
    }
}

impl Transport for LocalTransport {
    async fn send(&mut self, command: Command) -> Result<ResponseData> {
        crate::policy::enforce(&command)?;
        match command {
            Command::Capture { include, opts, url } => self.do_capture(include, opts, url).await,
            Command::Action { action, capture } => self.do_action(action, capture).await,
            Command::Eval { code } => self.do_eval(&code).await,
            Command::Wait {
                condition,
                timeout_ms,
            } => self.do_wait(condition, timeout_ms).await,
            Command::Status => self.do_status().await,
            Command::TabList => self.do_tab_list().await,
            Command::TabSwitch { tab_id } => self.do_tab_switch(&tab_id).await,
            Command::TabNew { url } => self.do_tab_new(&url).await,
            Command::TabClose { tab_id } => self.do_tab_close(&tab_id).await,
            Command::DomSet {
                selector,
                property,
                value,
            } => self.do_dom_set(&selector, property, &value).await,
            Command::DomGet { selector, property } => self.do_dom_get(&selector, property).await,
            Command::Fetch { url, method, body } => {
                self.do_fetch(&url, method.as_deref(), body.as_deref())
                    .await
            }
            Command::FrameList => self.do_frame_list().await,
            Command::FrameSwitch { selector } => self.do_frame_switch(selector).await,
            Command::CookieList { url } => self.do_cookie_list(&url).await,
            Command::CookieSet {
                url,
                name,
                value,
                http_only,
                secure,
            } => {
                self.do_cookie_set(&url, &name, &value, http_only, secure)
                    .await
            }
            Command::CookieDelete { url, name } => self.do_cookie_delete(&url, &name).await,
            Command::ConsoleStart => self.do_console_start().await,
            Command::ConsoleRead => self.do_console_read().await,
            Command::ConsoleClear => self.do_console_clear().await,
            Command::NetworkStart => self.do_network_start().await,
            Command::NetworkRead { since } => self.do_network_read(since).await,
            Command::NetworkClear => self.do_network_clear().await,
            Command::SessionExport => self.do_session_export().await,
            Command::SessionImport { data } => self.do_session_import(&data).await,
            Command::Ping => Ok(ResponseData::Pong),
        }
    }
}

// ── Active-frame persistence (survives across CLI invocations) ────────────

fn active_frame_file(browser_context_id: Option<&str>) -> PathBuf {
    let key = browser_context_id.unwrap_or("default");
    dirs::runtime_dir().join(format!("active_frame_{key}.json"))
}

fn read_persisted_active_frame(browser_context_id: Option<&str>) -> Option<String> {
    let path = active_frame_file(browser_context_id);
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str::<String>(&raw).ok()
}

pub(super) fn write_persisted_active_frame(browser_context_id: Option<&str>, frame_id: &str) {
    let path = active_frame_file(browser_context_id);
    if let Ok(s) = serde_json::to_string(frame_id) {
        let _ = std::fs::write(&path, s);
    }
}

pub(super) fn clear_persisted_active_frame(browser_context_id: Option<&str>) {
    let _ = std::fs::remove_file(active_frame_file(browser_context_id));
}

// ── Active-tab persistence ────────────────────────────────────────────────

fn active_tab_file(browser_context_id: Option<&str>) -> PathBuf {
    let key = browser_context_id.unwrap_or("default");
    dirs::runtime_dir().join(format!("active_tab_{key}.json"))
}

pub(super) fn read_persisted_active_tab(browser_context_id: Option<&str>) -> Option<String> {
    let path = active_tab_file(browser_context_id);
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str::<String>(&raw).ok()
}

pub(super) fn write_persisted_active_tab(browser_context_id: Option<&str>, target_id: &str) {
    let path = active_tab_file(browser_context_id);
    if let Ok(s) = serde_json::to_string(target_id) {
        let _ = std::fs::write(&path, s);
    }
}

pub(super) fn clear_persisted_active_tab(browser_context_id: Option<&str>) {
    let _ = std::fs::remove_file(active_tab_file(browser_context_id));
}

// ── Device-emulation persistence ──────────────────────────────────────────
//
// `Emulation.setDeviceMetricsOverride` survives a CDP client disconnect in
// headless Chrome, but `Emulation.setUserAgentOverride` reverts the moment the
// client that set it disconnects. Since every WebPilot CLI invocation is a fresh
// client that re-attaches to the one persistent Chrome, a UA override set in one
// process would silently vanish for the next — the asymmetry a field report
// surfaced. So the full emulation record is persisted per context and re-applied
// on every `open`, exactly like the armed monitors and the active frame: the
// emulation an agent set in one command holds across the next, metrics AND UA.

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct DeviceState {
    pub width: u32,
    pub height: u32,
    pub mobile: bool,
    pub scale: f64,
    pub user_agent: Option<String>,
}

impl DeviceState {
    /// Apply this emulation to a page session: metrics always, UA only when set.
    pub(crate) async fn apply(&self, page: &CdpClient) -> Result<()> {
        page.send(
            "Emulation.setDeviceMetricsOverride",
            Some(json!({
                "width": self.width,
                "height": self.height,
                "deviceScaleFactor": self.scale,
                "mobile": self.mobile,
            })),
        )
        .await?;
        if let Some(ua) = &self.user_agent {
            page.send(
                "Emulation.setUserAgentOverride",
                Some(json!({"userAgent": ua})),
            )
            .await?;
        }
        Ok(())
    }
}

fn device_state_file(browser_context_id: Option<&str>) -> PathBuf {
    let key = browser_context_id.unwrap_or("default");
    dirs::runtime_dir().join(format!("device_{key}.json"))
}

fn read_persisted_device(browser_context_id: Option<&str>) -> Option<DeviceState> {
    let raw = std::fs::read_to_string(device_state_file(browser_context_id)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub(crate) fn write_persisted_device(browser_context_id: Option<&str>, state: &DeviceState) {
    if let Ok(s) = serde_json::to_string(state) {
        let _ = std::fs::write(device_state_file(browser_context_id), s);
    }
}

pub(crate) fn clear_persisted_device(browser_context_id: Option<&str>) {
    let _ = std::fs::remove_file(device_state_file(browser_context_id));
}

// ── Armed-monitor persistence ─────────────────────────────────────────────
//
// `console start` / `network start` arm a per-context recording intent that
// must outlive the CLI process that issued it — the hooks live on the page's
// `window` and are wiped by every full-document navigation, so whichever
// later process drives a navigation is the one that has to re-install them.
// One marker file per monitor: file creation is atomic, so two processes
// arming different monitors concurrently can never lose each other's flag
// (a shared read-modify-write JSON could). Arming is monotonic within a
// session — nothing disarms short of `quit`, which removes the markers
// alongside the tab/frame pins.

#[derive(Copy, Clone)]
pub(super) enum Monitor {
    Console,
    Network,
}

impl Monitor {
    fn name(self) -> &'static str {
        match self {
            Monitor::Console => "console",
            Monitor::Network => "network",
        }
    }
}

fn monitor_marker(kind: Monitor, browser_context_id: Option<&str>) -> PathBuf {
    let key = browser_context_id.unwrap_or("default");
    dirs::runtime_dir().join(format!("monitor_{}_{key}", kind.name()))
}

fn read_persisted_monitors(browser_context_id: Option<&str>) -> (bool, bool) {
    (
        monitor_marker(Monitor::Console, browser_context_id).exists(),
        monitor_marker(Monitor::Network, browser_context_id).exists(),
    )
}

pub(super) fn persist_monitor_armed(kind: Monitor, browser_context_id: Option<&str>) {
    let _ = std::fs::write(monitor_marker(kind, browser_context_id), b"");
}

fn clear_persisted_monitors(browser_context_id: Option<&str>) {
    let _ = std::fs::remove_file(monitor_marker(Monitor::Console, browser_context_id));
    let _ = std::fs::remove_file(monitor_marker(Monitor::Network, browser_context_id));
}

async fn frame_exists(page: &CdpClient, frame_id: &str) -> bool {
    let Ok(tree) = page.send("Page.getFrameTree", None).await else {
        return false;
    };
    fn walk(node: &Value, target: &str) -> bool {
        if node
            .get("frame")
            .and_then(|f| f.get("id"))
            .and_then(|v| v.as_str())
            == Some(target)
        {
            return true;
        }
        if let Some(children) = node.get("childFrames").and_then(|v| v.as_array()) {
            for child in children {
                if walk(child, target) {
                    return true;
                }
            }
        }
        false
    }
    tree.get("frameTree")
        .map(|t| walk(t, frame_id))
        .unwrap_or(false)
}

// ── Module-private helpers ────────────────────────────────────────────────

async fn resolve_target(
    browser: &CdpClient,
    ws_url: &str,
    context: Option<&str>,
) -> Result<(CdpClient, Option<String>, String)> {
    if let Some(ctx_name) = context {
        // Context mode anchors on the context-entry's target id, but a
        // user-issued `tab switch` may have moved the active tab to a
        // sibling page within the same browser context. Honour the
        // persisted active_tab first when it points to a still-live page.
        let initial = local_context::resolve_context_target(browser, ctx_name).await?;
        let file_path = local_context::context_file_path(ctx_name);
        let browser_context_id = std::fs::read_to_string(&file_path)
            .ok()
            .and_then(|data| serde_json::from_str::<local_context::ContextEntry>(&data).ok())
            .map(|e| e.browser_context_id);

        let target_id = pick_active_target(browser, browser_context_id.as_deref(), Some(&initial))
            .await
            .unwrap_or(initial);

        let cdp = connect_to_page(ws_url, &target_id).await?;
        Ok((cdp, browser_context_id, target_id))
    } else {
        let target_id = pick_active_target(browser, None, None)
            .await
            .ok_or(WebPilotError::NoPage)?;
        let cdp = connect_to_page(ws_url, &target_id).await?;
        Ok((cdp, None, target_id))
    }
}

/// Pick the active page target, honouring `runtime/active_tab_<key>.json`
/// when its referent is still a live page; otherwise fall back to the
/// supplied default (context anchor) or the first page in the listing.
async fn pick_active_target(
    browser: &CdpClient,
    browser_context_id: Option<&str>,
    context_anchor: Option<&str>,
) -> Option<String> {
    let targets = browser.get_targets().await.ok()?;
    let is_page = |t: &&Value| t.get("type").and_then(|v| v.as_str()) == Some("page");
    let in_ctx = |t: &&Value| match browser_context_id {
        Some(id) => t.get("browserContextId").and_then(|v| v.as_str()) == Some(id),
        None => true,
    };
    let alive = |id: &str| -> bool {
        targets
            .iter()
            .any(|t| t.get("targetId").and_then(|v| v.as_str()) == Some(id) && is_page(&t))
    };

    if let Some(persisted) = read_persisted_active_tab(browser_context_id) {
        if alive(&persisted) {
            return Some(persisted);
        }
        clear_persisted_active_tab(browser_context_id);
    }

    if let Some(anchor) = context_anchor
        && alive(anchor)
    {
        return Some(anchor.to_string());
    }

    targets
        .iter()
        .find(|t| is_page(t) && in_ctx(t))
        .and_then(|t| {
            t.get("targetId")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
}

pub(super) async fn connect_to_page(ws_url: &str, target_id: &str) -> Result<CdpClient> {
    let authority = ws_url.split("/devtools/").next().unwrap_or(ws_url);
    let page_ws_url = format!("{authority}/devtools/page/{target_id}");
    let cdp = CdpClient::connect(&page_ws_url).await?;
    cdp.send("Page.enable", None).await?;
    cdp.send("Runtime.enable", None).await?;
    Ok(cdp)
}

fn find_page_in_context<'a>(
    targets: &'a [Value],
    browser_context_id: Option<&str>,
) -> Option<&'a Value> {
    targets.iter().find(|t| {
        t.get("type").and_then(|v| v.as_str()) == Some("page")
            && match browser_context_id {
                Some(id) => t.get("browserContextId").and_then(|v| v.as_str()) == Some(id),
                None => true,
            }
    })
}

pub(super) fn action_success(dom: Option<DomSnapshot>) -> ResponseData {
    ResponseData::Action {
        success: true,
        error: None,
        dom,
        url_changed: None,
        new_tab: None,
        capture_error: None,
    }
}

pub(super) fn artifact_path(prefix: &str, ext: &str) -> PathBuf {
    // Nanosecond stamp: parallel contexts minting artifacts in the same
    // millisecond must not share a filename and silently overwrite.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dirs::artifacts_dir().join(format!("{prefix}_{nanos}.{ext}"))
}

pub(super) fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Resolve the page target's top frame id from `Page.getFrameTree`. Stable for
/// the target's lifetime (a cross-origin navigation swaps the document, not the
/// frame id), so the caller caches it once at open.
async fn fetch_main_frame_id(page: &CdpClient) -> Result<String> {
    let tree = page.send("Page.getFrameTree", None).await?;
    tree.pointer("/frameTree/frame/id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            WebPilotError::Other {
                detail: "Page.getFrameTree returned no main frame id".into(),
            }
            .into()
        })
}

/// Register `bridge.js` to auto-load into the `BRIDGE_WORLD` isolated world on
/// every document, the current one included (`runImmediately`). The browser
/// content script's declarative injection, expressed for headless — so the
/// bridge is always present in its own world without per-call injection.
async fn install_bridge_world(page: &CdpClient) -> Result<()> {
    page.send(
        "Page.addScriptToEvaluateOnNewDocument",
        Some(json!({
            "source": BRIDGE_JS,
            "worldName": BRIDGE_WORLD,
            "runImmediately": true,
        })),
    )
    .await?;
    Ok(())
}

/// Subscribe to `Runtime.executionContext*` on the given page connection and
/// split each frame's contexts into the two maps the `LocalTransport` routes
/// against: the frame's default (`isDefault`) context is its MAIN world (page
/// expressions); the `BRIDGE_WORLD`-named context is where the bridge runs.
/// `Runtime.enable` is issued by `connect_to_page` before this spawns, so events
/// flow from first connection.
fn spawn_frame_context_listener(
    page: &CdpClient,
    main: Arc<Mutex<HashMap<String, String>>>,
    bridge: Arc<Mutex<HashMap<String, String>>>,
) {
    let mut events = page.subscribe_events();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let method = event.get("method").and_then(|v| v.as_str()).unwrap_or("");
                    match method {
                        "Runtime.executionContextCreated" => {
                            let ctx = event.pointer("/params/context");
                            let frame_id = ctx
                                .and_then(|c| c.pointer("/auxData/frameId"))
                                .and_then(|v| v.as_str())
                                .map(str::to_string);
                            let unique = ctx
                                .and_then(|c| c.get("uniqueId"))
                                .and_then(|v| v.as_str())
                                .map(str::to_string);
                            let is_default = ctx
                                .and_then(|c| c.pointer("/auxData/isDefault"))
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let is_bridge =
                                ctx.and_then(|c| c.get("name")).and_then(|v| v.as_str())
                                    == Some(BRIDGE_WORLD);
                            if let (Some(fid), Some(uid)) = (frame_id, unique) {
                                if is_default {
                                    main.lock().await.insert(fid, uid);
                                } else if is_bridge {
                                    bridge.lock().await.insert(fid, uid);
                                }
                            }
                        }
                        "Runtime.executionContextDestroyed" => {
                            // CDP identifies the gone context by its unique id;
                            // drop it so a later read can't hand back a context
                            // that no longer exists.
                            if let Some(uid) = event
                                .pointer("/params/executionContextUniqueId")
                                .and_then(|v| v.as_str())
                            {
                                main.lock().await.retain(|_, v| v != uid);
                                bridge.lock().await.retain(|_, v| v != uid);
                            }
                        }
                        "Runtime.executionContextsCleared" => {
                            main.lock().await.clear();
                            bridge.lock().await.clear();
                        }
                        _ => {}
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
}

/// Clear all per-context session state (active tab/frame markers, armed
/// monitors, device emulation) tied to `browser_context_id`. Callers that
/// dispose the CDP browser context should invoke this so no stale runtime
/// files remain.
pub(crate) fn clear_context_state(browser_context_id: &str) {
    clear_persisted_active_frame(Some(browser_context_id));
    clear_persisted_active_tab(Some(browser_context_id));
    clear_persisted_monitors(Some(browser_context_id));
    clear_persisted_device(Some(browser_context_id));
}

#[cfg(test)]
mod navigation_settled_tests {
    use super::*;
    use crate::test_support::{Reply, mock_cdp, ok};

    /// A mock that answers `Page.getFrameTree` with the given main-frame
    /// `loaderId`/`url` and `Runtime.evaluate` (document.readyState) with `ready`.
    fn serve_frame(
        loader: &'static str,
        url: &'static str,
        ready: &'static str,
    ) -> impl Fn(&Value) -> Reply {
        move |req| {
            let result = match req["method"].as_str() {
                Some("Page.getFrameTree") => {
                    json!({ "frameTree": { "frame": { "loaderId": loader, "url": url } } })
                }
                Some("Runtime.evaluate") => {
                    json!({ "result": { "type": "string", "value": ready } })
                }
                _ => json!({}),
            };
            Reply::Send(ok(req, result))
        }
    }

    #[tokio::test]
    async fn settled_when_loader_matches_and_document_ready() {
        let url = mock_cdp(serve_frame("L1", "https://same", "complete")).await;
        let cdp = CdpClient::connect(&url).await.unwrap();
        assert!(navigation_settled(&cdp, Some("L1"), "https://same").await);
    }

    #[tokio::test]
    async fn settled_when_url_left_before_url_even_if_loader_differs() {
        // A cross-page nav: the loader may differ across a process swap, but the
        // URL having moved is enough to count as committed.
        let url = mock_cdp(serve_frame("Lx", "https://new", "interactive")).await;
        let cdp = CdpClient::connect(&url).await.unwrap();
        assert!(navigation_settled(&cdp, Some("Lwant"), "https://old").await);
    }

    #[tokio::test]
    async fn not_settled_when_neither_loader_nor_url_committed() {
        let url = mock_cdp(serve_frame("Lx", "https://old", "complete")).await;
        let cdp = CdpClient::connect(&url).await.unwrap();
        assert!(!navigation_settled(&cdp, Some("Lwant"), "https://old").await);
    }

    #[tokio::test]
    async fn not_settled_while_document_still_loading() {
        let url = mock_cdp(serve_frame("L1", "https://same", "loading")).await;
        let cdp = CdpClient::connect(&url).await.unwrap();
        assert!(!navigation_settled(&cdp, Some("L1"), "https://same").await);
    }
}
