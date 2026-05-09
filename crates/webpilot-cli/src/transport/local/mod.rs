//! Headless transport — speaks CDP directly to a Chrome for Testing instance.
//!
//! `LocalTransport` is the in-process equivalent of the Native Messaging Host
//! plus the extension service worker plus the content bridge. It owns the
//! browser-level and page-level CDP connections, plus the cached target id and
//! optional browser-context id needed for multi-agent isolation.
//!
//! Bridge.js is injected lazily on first use of any `__webpilot_handle` call.
//! Per-frame execution-context routing is set up in `open` and rebound on
//! page swaps (navigation, tab switch).
//!
//! The `do_*` command handlers are split across sibling modules by domain:
//!   - `action`  — page-mutating actions (click/type/scroll/drag/navigate/...)
//!   - `capture` — DOM extraction, screenshot, PDF, accessibility tree
//!   - `query`   — eval, wait, dom get/set, fetch
//!   - `state`   — cookies, console + network monitoring, session, policies
//!   - `browser` — tab list/switch/new/close, frame list/switch, status

mod action;
mod browser;
mod capture;
mod query;
mod state;

use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
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

pub struct LocalTransport {
    pub(crate) browser: CdpClient,
    pub(crate) page: CdpClient,
    pub(crate) ws_url: String,
    pub(crate) browser_context_id: Option<String>,
    pub(crate) target_id: String,
    /// frame_id (CDP string) → execution context id. Populated by the
    /// background subscriber listening for `Runtime.executionContextCreated`.
    pub(crate) frame_contexts: Arc<Mutex<HashMap<String, i64>>>,
    /// Active frame for evaluation. `None` means the page's main world.
    pub(crate) active_frame_id: Arc<Mutex<Option<String>>>,
}

impl LocalTransport {
    /// Connect to a headless Chrome (launching one if needed) and resolve a
    /// page target. When `context_name` is `Some`, attaches to that context's
    /// page (creating the context on first call).
    pub async fn open(context_name: Option<&str>) -> Result<Self> {
        let ws_url = session::ensure_session().await?;
        let browser = CdpClient::connect(&ws_url).await?;

        let (page, browser_context_id, target_id) =
            resolve_target(&browser, &ws_url, context_name).await?;

        let frame_contexts = Arc::new(Mutex::new(HashMap::new()));
        spawn_frame_context_listener(&page, frame_contexts.clone());
        // `connect_to_page` already enabled Runtime, but its initial
        // `executionContextCreated` events were dispatched before the
        // listener subscribed and so were dropped by the broadcast channel.
        // Toggle the domain to force re-emission for every existing context.
        let _ = page.send("Runtime.disable", None).await;
        let _ = page.send("Runtime.enable", None).await;

        // Restore the active frame across CLI invocations. CLI calls are
        // separate processes, so without persistence `frames switch` would
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

        Ok(Self {
            browser,
            page,
            ws_url,
            browser_context_id,
            target_id,
            frame_contexts,
            active_frame_id: Arc::new(Mutex::new(restored_active)),
        })
    }

    pub(super) fn persisted_context_key(&self) -> Option<&str> {
        self.browser_context_id.as_deref()
    }

    pub fn page(&self) -> &CdpClient {
        &self.page
    }

    pub fn browser(&self) -> &CdpClient {
        &self.browser
    }

    // ── Frame routing ────────────────────────────────────────────────────

    /// Look up the active execution context id (None = main world).
    async fn active_context_id(&self) -> Option<i64> {
        let active = self.active_frame_id.lock().await;
        match active.as_deref() {
            Some(fid) => self.frame_contexts.lock().await.get(fid).copied(),
            None => None,
        }
    }

    /// Evaluate `expression` in the active frame's execution context (or the
    /// main world if no frame is active).
    pub(super) async fn eval_in_active(&self, expression: &str) -> Result<Value> {
        let mut params = json!({
            "expression": expression,
            "returnByValue": true,
            "awaitPromise": true,
        });
        if let Some(cid) = self.active_context_id().await {
            params["contextId"] = cid.into();
        }
        let result = self.page.send("Runtime.evaluate", Some(params)).await?;
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

    // ── Bridge invocation ────────────────────────────────────────────────

    async fn ensure_bridge(&self) -> Result<()> {
        let loaded = self
            .eval_in_active("typeof __webpilot_handle === 'function'")
            .await?;
        if loaded.as_bool() != Some(true) {
            let mut params = json!({
                "expression": BRIDGE_JS,
                "returnByValue": true,
            });
            if let Some(cid) = self.active_context_id().await {
                params["contextId"] = cid.into();
            }
            self.page.send("Runtime.evaluate", Some(params)).await?;
        }
        Ok(())
    }

    pub(super) async fn invoke_bridge(&self, msg: &Value) -> Result<Value> {
        self.ensure_bridge().await?;
        let payload = msg.to_string();
        let js = format!("(async () => __webpilot_handle({payload}))()");
        self.eval_in_active(&js).await
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

    pub(super) async fn rebind_frame_listener(&mut self) {
        self.frame_contexts = Arc::new(Mutex::new(HashMap::new()));
        spawn_frame_context_listener(&self.page, self.frame_contexts.clone());
        // Same race as `open`: re-emit existing executionContextCreated events.
        let _ = self.page.send("Runtime.disable", None).await;
        let _ = self.page.send("Runtime.enable", None).await;
    }

    pub(super) async fn navigate_reconnect(&mut self, url: &str) -> Result<()> {
        let before_url = self
            .browser
            .get_targets()
            .await
            .ok()
            .and_then(|targets| {
                targets
                    .iter()
                    .find(|t| {
                        t.get("targetId").and_then(|v| v.as_str()) == Some(&self.target_id)
                    })
                    .and_then(|t| t.get("url").and_then(|v| v.as_str()))
                    .map(str::to_string)
            })
            .unwrap_or_default();

        let _ = self
            .page
            .send_with_timeout(
                "Page.navigate",
                Some(json!({"url": url})),
                std::time::Duration::from_secs(3),
            )
            .await;

        let deadline = std::time::Instant::now() + crate::timeouts::navigation();
        loop {
            tokio::time::sleep(crate::timeouts::poll_interval()).await;

            if let Ok(targets) = self.browser.get_targets().await
                && let Some(page_target) =
                    find_page_in_context(&targets, self.browser_context_id.as_deref())
            {
                let current = page_target
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if current != before_url {
                    tokio::time::sleep(crate::timeouts::post_reconnect()).await;
                    break;
                }
            }
            if std::time::Instant::now() > deadline {
                return Err(WebPilotError::Timeout {
                    kind: "navigation".into(),
                    elapsed_ms: crate::timeouts::navigation().as_millis() as u64,
                }
                .into());
            }
        }

        let targets = self.browser.get_targets().await?;
        let target = find_page_in_context(&targets, self.browser_context_id.as_deref())
            .ok_or(WebPilotError::NoPage)?;
        let new_target_id = target
            .get("targetId")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let new_page = connect_to_page(&self.ws_url, &new_target_id).await?;

        let _ = new_page
            .wait_for_event(
                "Page.domContentEventFired",
                std::time::Duration::from_secs(5),
            )
            .await;
        tokio::time::sleep(crate::timeouts::post_navigate()).await;

        self.page = new_page;
        self.target_id = new_target_id;
        *self.active_frame_id.lock().await = None;
        self.rebind_frame_listener().await;
        Ok(())
    }
}

impl Transport for LocalTransport {
    async fn send(&mut self, command: Command) -> Result<ResponseData> {
        match command {
            Command::Capture {
                include,
                opts,
                url,
            } => self.do_capture(include, opts, url).await,
            Command::Action { action, capture } => self.do_action(action, capture).await,
            Command::Eval { code } => self.do_evaluate(&code).await,
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
                self.do_fetch(&url, method.as_deref(), body.as_deref()).await
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
            Command::PolicySet { action, verdict } => self.do_policy_set(action, verdict).await,
            Command::PolicyList => self.do_policy_list().await,
            Command::PolicyClear => self.do_policy_clear().await,
            Command::Ping => Ok(ResponseData::Pong),
        }
    }
}

// ── Active-frame persistence (survives across CLI invocations) ────────────

fn active_frame_file(browser_context_id: Option<&str>) -> PathBuf {
    let key = browser_context_id.unwrap_or("default");
    dirs::runtime_dir().join(format!("active_frame_{key}.json"))
}

pub(super) fn read_persisted_active_frame(browser_context_id: Option<&str>) -> Option<String> {
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
        // user-issued `tabs switch` may have moved the active tab to a
        // sibling page within the same browser context. Honour the
        // persisted active_tab first when it points to a still-live page.
        let initial = local_context::resolve_context_target(browser, ctx_name).await?;
        let file_path = local_context::context_file_path(ctx_name);
        let browser_context_id = std::fs::read_to_string(&file_path)
            .ok()
            .and_then(|data| serde_json::from_str::<local_context::ContextEntry>(&data).ok())
            .map(|e| e.browser_context_id);

        let target_id = pick_active_target(
            browser,
            browser_context_id.as_deref(),
            Some(&initial),
        )
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
        .and_then(|t| t.get("targetId").and_then(|v| v.as_str()).map(str::to_string))
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
    }
}

pub(super) fn artifact_path(prefix: &str, ext: &str) -> PathBuf {
    let dir = dirs::artifacts_dir();
    dir.join(format!("{prefix}_{}.{ext}", epoch_ms()))
}

pub(super) fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Subscribe to `Runtime.executionContextCreated`/`...Destroyed` on the given
/// page CDP connection. Maintains the frame_id → context_id map shared with
/// the owning `LocalTransport`. `Runtime.enable` is issued by `connect_to_page`
/// before this listener spawns, so events flow from first connection.
fn spawn_frame_context_listener(page: &CdpClient, map: Arc<Mutex<HashMap<String, i64>>>) {
    let mut events = page.subscribe_events();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let method = event.get("method").and_then(|v| v.as_str()).unwrap_or("");
                    match method {
                        "Runtime.executionContextCreated" => {
                            let frame_id = event
                                .pointer("/params/context/auxData/frameId")
                                .and_then(|v| v.as_str())
                                .map(str::to_string);
                            let cid =
                                event.pointer("/params/context/id").and_then(|v| v.as_i64());
                            if let (Some(fid), Some(c)) = (frame_id, cid) {
                                map.lock().await.insert(fid, c);
                            }
                        }
                        "Runtime.executionContextDestroyed" => {
                            let cid = event
                                .pointer("/params/executionContextId")
                                .and_then(|v| v.as_i64());
                            if let Some(c) = cid {
                                map.lock().await.retain(|_, v| *v != c);
                            }
                        }
                        "Runtime.executionContextsCleared" => {
                            map.lock().await.clear();
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

// ── Lifecycle (used by cli.rs Quit path) ─────────────────────────────────

/// Dispose a single named context's CDP browser context and remove its
/// on-disk state file. Returns `ContextNotFound` if no such context exists.
pub async fn quit_named_context(context_name: &str) -> Result<()> {
    let file_path = local_context::context_file_path(context_name);
    let data = std::fs::read_to_string(&file_path).map_err(|_| {
        WebPilotError::ContextNotFound {
            name: context_name.to_string(),
        }
    })?;
    let entry: local_context::ContextEntry = serde_json::from_str(&data)?;

    if let Some(ws_url) = session::get_existing_session()
        && let Ok(browser) = CdpClient::connect(&ws_url).await
    {
        let _ = browser
            .dispose_browser_context(&entry.browser_context_id)
            .await;
    }

    let _ = std::fs::remove_file(&file_path);
    clear_persisted_active_frame(Some(&entry.browser_context_id));
    clear_persisted_active_tab(Some(&entry.browser_context_id));
    Ok(())
}
