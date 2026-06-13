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

/// Max CDP frame-tree depth any recursive walk descends. The tree is
/// browser-supplied, and Chrome caps real nesting far below this, so a depth
/// limit well above any genuine page lets a pathological or corrupted tree
/// degrade (a shorter list / "not found") instead of overflowing the stack. One
/// source for every frame walk in this module and its siblings.
pub(super) const MAX_FRAME_DEPTH: u32 = 256;

pub struct LocalTransport {
    pub(crate) browser: CdpClient,
    pub(crate) page: CdpClient,
    pub(crate) ws_url: String,
    pub(crate) browser_context_id: Option<String>,
    pub(crate) target_id: String,
    /// `Some(dead_id)` when this transport opened onto a fallback survivor because
    /// the persisted pin had closed. `send` then refuses any command that would
    /// ACT on the active page (TabNotFound), so a page action never silently runs
    /// on the retargeted tab; tab management and status proceed so the agent can
    /// re-pin. Cleared on the next process (the dead pin was already dropped).
    pub(crate) pin_vanished: Option<String>,
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
    /// Shared lock on the named context's liveness file, held for this
    /// transport's whole lifetime (`None` for the default context). It is the
    /// signal a concurrent `gc_expired_contexts` probes with a non-blocking
    /// *exclusive* attempt: while any transport holds the shared lock the sweep
    /// skips the context, so a long-lived session (an MCP server reusing one
    /// transport past the idle TTL) is never disposed out from under itself.
    /// Being shared, it never blocks another resolve — only the GC's disposal.
    /// Dropped with the transport, which frees the context to be reaped once
    /// truly idle.
    _context_lock: Option<std::fs::File>,
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
        let (browser, ws_url) = match CdpClient::connect(&ws_url).await {
            Ok(browser) => (browser, ws_url),
            Err(_) => {
                // Invalidate only if this is still the session we failed on —
                // a concurrent `open` may have already relaunched a fresh one.
                session::invalidate_session_if_current(&ws_url);
                // Carry the RELAUNCHED url out of the match: it is the one
                // `browser` just connected to, so resolve_target's page connection
                // and the stored `ws_url` must use it, not the dead session's URL.
                let ws_url = session::ensure_session().await?;
                let browser = CdpClient::connect(&ws_url).await?;
                (browser, ws_url)
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

        let (page, browser_context_id, target_id, context_lock, pin_vanished) =
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
        // `device set --user-agent` an agent issued would be silently gone. But
        // honor the policy gate the `device` command itself respects: under
        // `default deny` a previously-set emulation (UA spoof especially) must NOT
        // be restored into a locked-down process — the persisted state stays on
        // disk for when `device` is re-allowed.
        if !crate::policy::denies(webpilot::types::PolicyKey::Device)
            && let Some(dev) = read_persisted_device(browser_context_id.as_deref())
            && let Err(e) = dev.apply(&page).await
        {
            // Re-apply failing must not be SILENT: the agent set this emulation
            // (UA/viewport — identity-shaping) and a session quietly running
            // without it would lie about what the page sees. But it must not
            // fail the open either — that would block every command including
            // the `device reset` that recovers. Warn (stderr) and continue.
            tracing::warn!(
                "device emulation could not be re-applied: {e} — the page sees the REAL \
                 user agent/viewport; run `webpilot device set …` again or `webpilot \
                 device reset` to clear the persisted emulation"
            );
        }

        // Restore the active frame across CLI invocations VERBATIM. CLI calls are
        // separate processes, so without persistence `frame switch` would be a
        // no-op — the next `eval` would lose the active frame and silently fall
        // back to the main world. A frame that has since VANISHED is deliberately
        // KEPT, not dropped: a scoped command then resolves it as `FrameNotFound`
        // (exit 4 → recapture / re-switch) instead of SILENTLY running in the main
        // frame, matching browser mode (which keeps `activeFrameId` and
        // FrameNotFounds via `frameVanishedError`). The clear is deferred to the
        // recovery paths that REPORT it — `frame list` re-validates against the
        // live tree and returns the reset, `frame main` / a fresh `frame switch`
        // replace it — so the agent is never silently retargeted on open.
        let restored_active = read_persisted_active_frame(browser_context_id.as_deref());

        // Restore the armed-monitor state across CLI invocations: `network
        // start` in one process must keep recording through navigations run
        // from later processes, exactly as the browser-mode service worker's
        // per-tab monitoring survives because the worker itself persists.
        let (console_armed, network_armed) = read_persisted_monitors(browser_context_id.as_deref());

        let transport = Self {
            browser,
            page,
            ws_url,
            browser_context_id,
            target_id,
            pin_vanished,
            main_frame_id,
            frame_contexts,
            bridge_contexts,
            active_frame_id: Arc::new(Mutex::new(restored_active)),
            console_monitoring: Arc::new(AtomicBool::new(console_armed)),
            network_monitoring: Arc::new(AtomicBool::new(network_armed)),
            _context_lock: context_lock,
        };

        // Re-arm armed monitors against the CURRENT document, mirroring the
        // device re-apply above. The hooks live on `window` and survive process
        // boundaries, so restoring the flag is enough WHILE the document stays
        // put — but a navigation WebPilot did NOT drive (a page-initiated
        // redirect between two CLI commands) wipes them with no
        // `reinstall_monitors` to fire, so an armed monitor would silently stop
        // recording until the next WebPilot-driven navigation. Re-applying on
        // open keeps an armed monitor following the page across out-of-band
        // navigations too. The install JS is idempotent and buffer-preserving
        // (it re-patches only an unpatched `console`/`fetch` and keeps an
        // existing buffer), so re-arming an already-hooked document is a no-op,
        // and it re-checks policy, so an `eval` deny still stops it.
        if console_armed || network_armed {
            transport.reinstall_monitors().await;
        }

        Ok(transport)
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

    /// Force the Runtime to re-emit `executionContextCreated` for every existing
    /// context and wait, briefly, until each of `frame_ids` has its MAIN-world
    /// context recorded. The async listener can lag a navigation, so without
    /// this a frame whose context simply hasn't landed yet would be acted on (or
    /// silently skipped) as if it had no MAIN world. Best-effort: returns once
    /// all are present or the short budget expires. Used before a `frame find`
    /// predicate judges every candidate, and after a frame switch.
    async fn settle_frame_contexts(&self, frame_ids: &[String]) {
        // Drop any existing entry for these frames first. After a navigation the
        // map can still hold a *stale* uniqueContextId — the `executionContext\
        // Destroyed` event that prunes it is processed asynchronously — and
        // `Runtime.disable`/`enable` does not emit `executionContextsCleared`, so
        // a bare `contains_key` would treat that dead id as "settled" and the
        // caller would evaluate against a context CDP has already discarded.
        // Clearing makes "settled" mean a context *re-observed* after the re-emit
        // below, not a leftover from before the navigation.
        {
            let mut map = self.frame_contexts.lock().await;
            for fid in frame_ids {
                map.remove(fid);
            }
        }
        let _ = self.page.send("Runtime.disable", None).await;
        let _ = self.page.send("Runtime.enable", None).await;
        for _ in 0..20 {
            let all_present = {
                let map = self.frame_contexts.lock().await;
                frame_ids.iter().all(|fid| map.contains_key(fid))
            };
            if all_present {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
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
        match self.eval_in_context(expression, cid.as_deref(), true).await {
            Ok(result) => Ok(decode_eval_result(&result)),
            Err(e) if is_stale_context(&e) => {
                // The active frame's MAIN-world context was destroyed by a
                // navigation, but the map can still hand back that dead id until
                // the async `executionContextDestroyed` lands — including right
                // after a structural `frame switch` that bound a since-navigated
                // frame. Drop the stale id and re-resolve (`active_context_id`
                // then waits for the new document's context), retrying once: the
                // MAIN-world mirror of the bridge path's renderer-swap retry.
                if let Some(stale) = &cid {
                    self.frame_contexts.lock().await.retain(|_, v| v != stale);
                }
                let fresh = self.active_context_id().await?;
                let result = self
                    .eval_in_context(expression, fresh.as_deref(), true)
                    .await?;
                Ok(decode_eval_result(&result))
            }
            Err(e) => Err(e),
        }
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
        // Hand the message to the bridge via `JSON.parse`, NOT as an inlined
        // object literal: `__webpilot_handle({…})` evaluates the payload as
        // source, where `{"__proto__": v}` is a prototype SETTER that silently
        // drops a real `__proto__` key (a `session import` storage key the page
        // legitimately had), and inlining external data (a session file's
        // values) as code is needlessly injection-adjacent even in the isolated
        // world. `JSON.parse` treats every key as data, `__proto__` included.
        // Double-encode so the JSON string is itself a valid JS string literal.
        let payload = serde_json::to_string(&msg.to_string()).expect("string serializes");
        let js = format!("(async () => __webpilot_handle(JSON.parse({payload})))()");

        let cid = self.bridge_context_id().await?;
        match self
            .eval_in_context_with_timeout(&js, Some(&cid), true, timeout)
            .await
        {
            Ok(result) => Ok(decode_eval_result(&result)),
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
                Ok(decode_eval_result(&result))
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
        self.await_live_bridge_context().await;
        Ok(())
    }

    /// Block until the main frame's bridge-world context names the COMMITTED,
    /// PARSED document. Right after a navigation the `executionContextCreated`
    /// for the new document's bridge world can land a poll-cycle after the
    /// transitional pre-commit context, and `await_context` returns whichever is
    /// mapped first — so a snapshot fired the instant a navigation settles can run
    /// against the empty pre-load document and come back blank. Resolve the bridge
    /// context, verify its OWN `location.href` matches the live frame and its
    /// document has parsed, and drop any context that does not so the resolver
    /// re-waits for the real one. Bounded by the navigation budget.
    async fn await_live_bridge_context(&self) {
        // The URL the bridge must agree it is live ON must carry the fragment, the
        // way `location.href` does. `Page.getFrameTree`'s `Frame.url` STRIPS the
        // fragment (CDP carries it separately in `urlFragment`), so comparing a
        // frame-tree url against the bridge's `location.href` never matched for a
        // `#fragment` navigation — the only bridge context was evicted every poll
        // until the navigation timeout, after which the capture failed as
        // `FrameNotFound`. `bound_target_url` (Target.getTargets) carries the
        // fragment, exactly as the rest of the transport's URL comparisons do.
        let want = self.bound_target_url().await;
        if want.is_empty() {
            return;
        }
        let deadline = std::time::Instant::now() + webpilot::settings::timeouts().navigation;
        loop {
            if std::time::Instant::now() >= deadline {
                return;
            }
            if let Ok(cid) = self.bridge_context_id().await {
                let live = self
                    .eval_in_context_with_timeout(
                        "JSON.stringify([location.href, document.readyState])",
                        Some(&cid),
                        true,
                        PROBE,
                    )
                    .await
                    .ok()
                    .and_then(|v| decode_eval_result(&v).as_str().map(str::to_owned))
                    .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
                    .is_some_and(|a| a.len() == 2 && a[0] == want && a[1] != "loading");
                if live {
                    return;
                }
                self.bridge_contexts.lock().await.retain(|_, v| *v != cid);
            }
            tokio::time::sleep(webpilot::settings::timeouts().poll_interval).await;
        }
    }

    /// After a click navigated the switched iframe, wait until its bridge context
    /// names a NEW, parsed document. The navigation replaced the frame's execution
    /// context, so a context id different from `before` with a `readyState` past
    /// `loading` is the signal the new page is ready — without it the snapshot is
    /// the pre-click document, since an iframe-internal navigation never touches
    /// the top URL the main settle watches. Bounded by the navigation budget.
    async fn await_live_active_frame_context(&self, before: Option<String>) {
        let deadline = std::time::Instant::now() + webpilot::settings::timeouts().navigation;
        loop {
            if std::time::Instant::now() >= deadline {
                return;
            }
            if let Ok(cid) = self.bridge_context_id().await
                && Some(&cid) != before.as_ref()
            {
                let parsed = self
                    .eval_in_context_with_timeout("document.readyState", Some(&cid), true, PROBE)
                    .await
                    .ok()
                    .and_then(|v| decode_eval_result(&v).as_str().map(str::to_owned))
                    .is_some_and(|s| s != "loading");
                if parsed {
                    return;
                }
                // A transitional context that hasn't parsed — drop it so the next
                // poll re-resolves once the listener records the committed one.
                self.bridge_contexts.lock().await.retain(|_, v| *v != cid);
            }
            tokio::time::sleep(webpilot::settings::timeouts().poll_interval).await;
        }
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

        // ERR_ABORTED is a STAY-PUT, not a failure: a 204/download/intercepted
        // load aborts without committing a new document, leaving the previous page
        // live and capturable. Bound its wait by the short PROBE — a TRANSITIONAL
        // abort (a swap that supersedes the load) commits a new document within it
        // and settles below; a TERMINAL one never commits, and spinning to the
        // full navigation deadline only delays a false NavigationFailed. (A
        // concrete net error already returned above; a send `Err` keeps the long
        // deadline so a real socket drop isn't masked.)
        let mut aborted = matches!(
            &start_error,
            Some(WebPilotError::NavigationFailed { reason, .. }) if reason == "net::ERR_ABORTED"
        );
        let mut deadline = std::time::Instant::now()
            + if aborted {
                PROBE
            } else {
                webpilot::settings::timeouts().navigation
            };
        let loader = loader_id.as_deref();
        loop {
            if let Some((target_id, target_url)) = self.bound_target().await {
                if target_id != self.target_id {
                    // The tab being navigated is gone (closed by another process
                    // mid-navigation) and `bound_target` fell back to an UNRELATED
                    // sole sibling. A cross-tab id never names this navigation's
                    // result, so rebinding to it would silently retarget the agent
                    // — fail loud instead. (A same-tab nav, even cross-process,
                    // keeps its target id, so the live navigation never trips this.)
                    return Err(WebPilotError::NavigationFailed {
                        url: before_url.to_string(),
                        reason: "the tab being navigated was closed".into(),
                    }
                    .into());
                }
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
                if aborted {
                    // A TRANSITIONAL abort's commit can land in the poll→deadline
                    // gap. Take one more reading before declaring stay-put: if the
                    // main-tab URL moved (a cross-page commit), it was transitional
                    // after all — re-enter the loop on the FULL deadline so the
                    // cross-page branch rebinds the new document. Only a confirmed
                    // no-move is the TERMINAL stay-put (a 204/download/intercepted
                    // load): the main frame stopped without committing, so the
                    // PREVIOUS document is live and capturable — return success on
                    // it, not a 15s spin to a false NavigationFailed.
                    let moved = if let Some((tid, turl)) = self.bound_target().await {
                        tid == self.target_id && turl != before_url
                    } else {
                        false
                    };
                    if moved {
                        aborted = false;
                        deadline =
                            std::time::Instant::now() + webpilot::settings::timeouts().navigation;
                        continue;
                    }
                    self.clear_active_frame().await;
                    self.reinstall_monitors().await;
                    return Ok(());
                }
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

    /// Whether the switched-into frame is still in the tree. `true` when no frame
    /// is active (nothing to lose) or it is still present; `false` only when a
    /// frame was switched and the document that held it is gone — so the caller
    /// resets the scope instead of resolving a dead context. An unreadable tree
    /// returns `true`: don't tear down a live scope on an uncertain read.
    async fn active_frame_still_present(&self) -> bool {
        let Some(fid) = self.active_frame_id.lock().await.clone() else {
            return true;
        };
        let Ok(tree) = self.page.send("Page.getFrameTree", None).await else {
            return true;
        };
        fn contains(node: &Value, fid: &str, depth: u32) -> bool {
            if depth > MAX_FRAME_DEPTH {
                return false;
            }
            node.pointer("/frame/id").and_then(Value::as_str) == Some(fid)
                || node
                    .get("childFrames")
                    .and_then(Value::as_array)
                    .is_some_and(|kids| kids.iter().any(|k| contains(k, fid, depth + 1)))
        }
        tree.get("frameTree")
            .is_some_and(|root| contains(root, &fid, 0))
    }

    /// `(target_id, url)` of the page WebPilot is bound to — the target it just
    /// navigated, identified by `self.target_id`. If that exact target is gone
    /// (a swap replaced it), it falls back to the context's page ONLY when that
    /// page is unique: with several pages, which one is this navigation's result
    /// is unknowable, so it returns `None` rather than rebind to a sibling tab.
    async fn bound_target(&self) -> Option<(String, String)> {
        let targets = self.browser.get_targets().await.ok()?;
        let exact = targets
            .iter()
            .find(|t| t.get("targetId").and_then(|v| v.as_str()) == Some(&self.target_id));
        let pick = match exact {
            Some(t) => t,
            None => {
                // The exact target vanished — fall back to the sole page in our
                // context. The created-context list is read only here (the fast
                // path never needs it) and fails closed: a read error means scope
                // can't be determined, so don't guess a sibling tab.
                let created = self.browser.get_browser_contexts().await.ok()?;
                sole_page_in_context(&targets, self.browser_context_id.as_deref(), &created)?
            }
        };
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

/// Decode a CDP `RemoteObject` into a faithful value. CDP omits `value` for
/// results JSON cannot carry — `undefined`, `NaN`/`±Infinity`/`-0`, `BigInt`,
/// functions, symbols. Collapsing all of those to `null` would tell the agent
/// the page returned `null` when it did not (a property read that is `undefined`
/// is not the same as one that is `null`). So fall back to the engine's own
/// rendering: the `unserializableValue` literal (`"NaN"`, `"42n"`), the bare
/// type name for `undefined`, or the object `description` (a function/symbol),
/// and only a genuinely empty object to `null`.
fn decode_eval_result(remote: &Value) -> Value {
    if let Some(value) = remote.get("value") {
        return value.clone();
    }
    if let Some(unserializable) = remote.get("unserializableValue").and_then(Value::as_str) {
        return Value::String(unserializable.to_owned());
    }
    if remote.get("type").and_then(Value::as_str) == Some("undefined") {
        return Value::String("undefined".to_owned());
    }
    remote
        .get("description")
        .filter(|d| d.is_string())
        .cloned()
        .unwrap_or(Value::Null)
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
        // The pinned tab closed and this transport attached to a fallback
        // survivor. A command that ACTS on the active page must not SILENTLY run
        // on it — fail loud with TabNotFound, carrying the dead id, so the agent
        // re-pins. Tab management and status only need the browser connection, so
        // they proceed and let the recovery happen. The flag is CONSUMED by that
        // one loud failure: the persisted pin is already dropped, so for a CLI
        // process the next invocation re-resolves onto the fallback anyway — and
        // a long-lived transport (the MCP server) must behave identically, not
        // repeat TabNotFound forever on a flag no later command can clear (which
        // blocked even `navigate`, leaving the session unrecoverable). One loud
        // signal, then the fallback is the active page — announced, not silent.
        if command_needs_active_page(&command)
            && let Some(dead) = self.pin_vanished.take()
        {
            return Err(WebPilotError::TabNotFound { tab_id: dead }.into());
        }
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
            Command::TabSwitch { tab_id } => self.do_tab_switch(&tab_id, true).await,
            Command::TabNew { url } => self.do_tab_new(&url).await,
            Command::TabClose { tab_id } => self.do_tab_close(&tab_id).await,
            Command::DomSet {
                selector,
                property,
                value,
            } => self.do_dom_set(&selector, property, &value).await,
            Command::DomGet { selector, property } => self.do_dom_get(&selector, property).await,
            Command::Fetch {
                url,
                method,
                body,
                headers,
            } => {
                self.do_fetch(&url, method.as_deref(), body.as_deref(), &headers)
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
                same_site,
                expires,
            } => {
                self.do_cookie_set(state::CookieSetSpec {
                    url: &url,
                    name: &name,
                    value: &value,
                    http_only,
                    secure,
                    same_site,
                    expires,
                })
                .await
            }
            Command::CookieDelete { url, name } => self.do_cookie_delete(&url, &name).await,
            Command::ConsoleStart => self.do_console_start().await,
            Command::ConsoleRead { since } => self.do_console_read(since).await,
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

pub(super) fn write_persisted_active_frame(
    browser_context_id: Option<&str>,
    frame_id: &str,
) -> std::io::Result<()> {
    let path = active_frame_file(browser_context_id);
    let s = serde_json::to_string(frame_id).expect("a frame-id string serializes");
    dirs::atomic_write(&path, s.as_bytes())
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

pub(super) fn write_persisted_active_tab(
    browser_context_id: Option<&str>,
    target_id: &str,
) -> std::io::Result<()> {
    let path = active_tab_file(browser_context_id);
    let s = serde_json::to_string(target_id).expect("a target-id string serializes");
    // Atomic temp+rename: a concurrent process resolving the active tab
    // (`pick_active_target`) must never read a torn/empty pin — that would parse
    // as `None` and silently retarget. The error is returned, not swallowed: a
    // failed write means the pin a `tab switch` exists to set never landed, so
    // the next process would attach to the wrong tab — the command must say so.
    dirs::atomic_write(&path, s.as_bytes())
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
    /// Apply this emulation to a page session: metrics, touch, AND user agent are
    /// all set unconditionally, so a `device set` fully defines the device and
    /// never leaves a prior override's stale remnant behind.
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
        // Touch is a separate override from the metrics `mobile` flag: without it
        // a mobile preset still reports `navigator.maxTouchPoints === 0`, so a
        // page's touch detection serves the desktop layout. Match touch to the
        // device — enabled (multi-touch) for mobile, off for desktop.
        page.send(
            "Emulation.setTouchEmulationEnabled",
            Some(json!({
                "enabled": self.mobile,
                "maxTouchPoints": 5,
            })),
        )
        .await?;
        // Always set the UA override, like the metrics and touch above: a
        // `device set` defines the WHOLE device, so an absent `--user-agent` means
        // the default UA and must CLEAR any prior override (`""` clears it, the
        // same value `device reset` sends) — not silently leave a stale one. Going
        // from a UA-bearing preset back to one without it would otherwise keep the
        // old UA active, contradicting the new device.
        page.send(
            "Emulation.setUserAgentOverride",
            Some(json!({"userAgent": self.user_agent.as_deref().unwrap_or("")})),
        )
        .await?;
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

pub(crate) fn write_persisted_device(
    browser_context_id: Option<&str>,
    state: &DeviceState,
) -> std::io::Result<()> {
    let s = serde_json::to_string(state).expect("device state serializes");
    dirs::atomic_write(&device_state_file(browser_context_id), s.as_bytes())
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

pub(super) fn persist_monitor_armed(
    kind: Monitor,
    browser_context_id: Option<&str>,
) -> std::io::Result<()> {
    // Presence is the whole signal (an empty file), so the write needs no
    // atomicity — but it must not be swallowed: this marker is what makes later
    // CLI processes re-arm the monitor, so a failed write means `console start`
    // didn't persist and the next process would silently run with no monitor.
    std::fs::write(monitor_marker(kind, browser_context_id), b"")
}

fn clear_persisted_monitors(browser_context_id: Option<&str>) {
    let _ = std::fs::remove_file(monitor_marker(Monitor::Console, browser_context_id));
    let _ = std::fs::remove_file(monitor_marker(Monitor::Network, browser_context_id));
}

// ── Module-private helpers ────────────────────────────────────────────────

async fn resolve_target(
    browser: &CdpClient,
    ws_url: &str,
    context: Option<&str>,
) -> Result<(
    CdpClient,
    Option<String>,
    String,
    Option<std::fs::File>,
    Option<String>,
)> {
    if let Some(ctx_name) = context {
        // Context mode anchors on the context-entry's target id, but a
        // user-issued `tab switch` may have moved the active tab to a
        // sibling page within the same browser context. Honour the
        // persisted active_tab first when it points to a still-live page.
        // `resolve_context_target` already holds the live lock and knows the
        // browser context it resolved or created, so take its id directly. A
        // re-read of the metadata file here could fail (a torn read, a parse
        // slip) and silently degrade to `None`, which would unlock the
        // any-page fallback in `pick_active_target` and break context isolation.
        let (initial, browser_context_id, context_lock) =
            local_context::resolve_context_target(browser, ctx_name).await?;

        let (target_id, vanished) =
            pick_active_target(browser, Some(&browser_context_id), Some(&initial)).await?;

        let cdp = connect_to_page(ws_url, &target_id).await?;
        Ok((
            cdp,
            Some(browser_context_id),
            target_id,
            Some(context_lock),
            vanished,
        ))
    } else {
        let (target_id, vanished) = pick_active_target(browser, None, None).await?;
        let cdp = connect_to_page(ws_url, &target_id).await?;
        Ok((cdp, None, target_id, None, vanished))
    }
}

/// Pick the active page target. A still-live persisted active tab
/// (`runtime/active_tab_<key>.json`) is the pin and wins. A persisted pin whose
/// page has since closed is a typed `TabNotFound`, never a silent retarget onto
/// some other tab — the same contract browser mode enforces; the agent must
/// `tab switch`/`tab new` to choose a new one. With NO pin (a fresh attach), the
/// context anchor or the first page in scope is taken.
/// Whether a command ACTS on the active page target (vs only needing the browser
/// connection). After the pinned tab vanished the transport attaches to a
/// fallback; these must NOT run on it (that is the silent retarget the pin
/// contract forbids). Tab management (list/new/switch/close) and status read only
/// the browser, so they proceed and let the agent re-pin. A new command defaults
/// to needing the page — the safe choice (fail loud) until classified otherwise.
fn command_needs_active_page(command: &Command) -> bool {
    match command {
        Command::TabList
        | Command::TabNew { .. }
        | Command::TabSwitch { .. }
        | Command::TabClose { .. }
        | Command::Status => false,
        // The cookie jar is browser-global: list/set/delete take their scope
        // from the URL argument, not from the active page (the Network calls
        // ride whatever target session is attached — same jar either way).
        // Browser mode routes these through `chrome.cookies` with no tab
        // resolution at all; a vanished pin must not block them here either.
        Command::CookieList { .. } | Command::CookieSet { .. } | Command::CookieDelete { .. } => {
            false
        }
        // Cookies are browser-global — set through any target's session they
        // land in the shared jar — so a cookie-only session import must not be
        // blocked by a vanished pin. Only the storage half writes into the
        // ACTIVE page's origin (via the bridge), so the import is page-bound
        // exactly when the payload carries storage: the same `hasStorage` gate
        // the browser worker applies, read through the shared predicate so the
        // two can never disagree. An unparsable payload classifies page-free so
        // the import's own schema error wins over a misleading TabNotFound.
        Command::SessionImport { data } => serde_json::from_str::<Value>(data)
            .map(|v| state::storage_to_import(&v))
            .unwrap_or(false),
        _ => true,
    }
}

/// Returns `(target_id, vanished_pin)`. `vanished_pin` is `Some(dead_id)` when the
/// persisted pin had closed: the transport still attaches to a fallback survivor
/// so pin-independent commands (tab list/new/switch/close, status) keep working,
/// while `send` turns `vanished_pin` into a typed `TabNotFound` for any command
/// that would ACT on the now-gone active page — so the never-silently-retarget
/// contract holds without blocking the agent's recovery. (Browser mode's
/// persistent worker reads `activeTabId` directly and never trips this.)
async fn pick_active_target(
    browser: &CdpClient,
    browser_context_id: Option<&str>,
    context_anchor: Option<&str>,
) -> Result<(String, Option<String>)> {
    let targets = browser.get_targets().await?;
    let is_page = |t: &&Value| t.get("type").and_then(|v| v.as_str()) == Some("page");
    let alive = |id: &str| -> bool {
        targets
            .iter()
            .any(|t| t.get("targetId").and_then(|v| v.as_str()) == Some(id) && is_page(&t))
    };

    let mut vanished = None;
    if let Some(persisted) = read_persisted_active_tab(browser_context_id) {
        if alive(&persisted) {
            return Ok((persisted, None));
        }
        // The pinned tab closed. Drop the dead pin and remember it: a list/switch
        // can still resolve a fallback below, but a page action must not silently
        // run on it — `send` raises TabNotFound for those, carrying this id.
        clear_persisted_active_tab(browser_context_id);
        vanished = Some(persisted);
    }

    if let Some(anchor) = context_anchor
        && alive(anchor)
    {
        return Ok((anchor.to_string(), vanished));
    }

    // A fresh attach: take the first page in scope. The created-context list is
    // read only here — the persisted-pin and anchor fast paths never need it — and
    // fails closed: a read error aborts rather than widen scope to every context.
    let created = browser.get_browser_contexts().await?;
    let target = targets
        .iter()
        .find(|t| is_page(t) && target_in_context(t, browser_context_id, &created))
        .and_then(|t| {
            t.get("targetId")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });
    match target {
        Some(target) => Ok((target, vanished)),
        // Zero pages in scope — the last tab was closed. Failing NoPage here
        // would wedge the session permanently: every command (including the
        // `tab new` and `navigate` that would fix it) needs this attach first,
        // and the NoPage guidance ("navigate") would point at a command that
        // fails the same way. Create a blank page to attach to instead — the
        // exact state a fresh browser starts in — so the recovery commands
        // work; the dead-pin signal still fires its one loud TabNotFound for a
        // page action, so nothing acts on the blank silently.
        None => {
            let target = browser
                .create_target("about:blank", browser_context_id)
                .await?;
            Ok((target, vanished))
        }
    }
}

pub(super) async fn connect_to_page(ws_url: &str, target_id: &str) -> Result<CdpClient> {
    let authority = ws_url.split("/devtools/").next().unwrap_or(ws_url);
    let page_ws_url = format!("{authority}/devtools/page/{target_id}");
    let cdp = CdpClient::connect(&page_ws_url).await?;
    cdp.send("Page.enable", None).await?;
    // With Page enabled, Chrome stops auto-dismissing javascript dialogs and
    // waits for an answer — an unanswered alert() would wedge the renderer.
    // Accept-with-default mirrors the browser-mode dialog override, so a page
    // branching on confirm()/prompt() behaves identically in both modes.
    cdp.spawn_dialog_responder();
    cdp.send("Runtime.enable", None).await?;
    Ok(cdp)
}

/// Whether a CDP target belongs to the given browser context. An isolated
/// `--context` matches its own created id; the default (`None`) matches every
/// target NOT in a created context (the `created` ids come from
/// `get_browser_contexts`, which lists every context EXCEPT Chrome's built-in
/// default — and default-context targets DO carry a browserContextId, just one
/// not in that list). Scoping every target lookup by this keeps an agent without
/// `--context` from ever seeing or attaching to a tab an isolated-context agent
/// opened, and the reverse — without it the default scope matched every context's
/// tabs (an isolation breach across multi-agent contexts).
pub(super) fn target_in_context(
    t: &Value,
    browser_context_id: Option<&str>,
    created: &[String],
) -> bool {
    let target_ctx = t.get("browserContextId").and_then(|v| v.as_str());
    match browser_context_id {
        Some(id) => target_ctx == Some(id),
        None => target_ctx.is_none_or(|c| !created.iter().any(|x| x == c)),
    }
}

/// The *sole* page in the browsing context, or `None` when there are zero or
/// more than one. `bound_target` uses this only as a fallback after its exact
/// target id has vanished: with exactly one page left it is unambiguously the
/// navigation's result, but with several there is no way to tell which is — so
/// it refuses to guess rather than silently rebind to a sibling tab.
fn sole_page_in_context<'a>(
    targets: &'a [Value],
    browser_context_id: Option<&str>,
    created: &[String],
) -> Option<&'a Value> {
    let mut pages = targets.iter().filter(|t| {
        t.get("type").and_then(|v| v.as_str()) == Some("page")
            && target_in_context(t, browser_context_id, created)
    });
    let only = pages.next()?;
    if pages.next().is_some() {
        return None;
    }
    Some(only)
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
