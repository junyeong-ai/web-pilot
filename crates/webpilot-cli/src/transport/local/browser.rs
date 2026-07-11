//! Browser-level commands: tabs, frames, status. These operate on the
//! browser-wide CDP target (or query the page's frame tree).

use anyhow::Result;
use serde_json::{Value, json};
use webpilot::WebPilotError;
use webpilot::protocol::{FrameSelector, ResponseData, RunMode};
use webpilot::types::{FrameInfo, TabInfo};

use super::{LocalTransport, action_success, attach_to_page, target_in_context};

/// A frame selector that matched more than one frame is ambiguous: switching
/// into whichever came first in document order would silently scope every
/// later command to a frame the agent may not have meant. Fail loud with the
/// match list so the agent refines the selector — the same contract every
/// selector kind holds, predicates included.
fn ambiguous_frame_switch(hits: &[&FrameInfo], what: String) -> ResponseData {
    // Cap each URL like `frame list` does — a data-URI iframe among the matches
    // would otherwise flood the error message with megabytes of base64.
    let urls = hits
        .iter()
        .map(|f| webpilot::types::line_safe_clip(&f.url, 200))
        .collect::<Vec<_>>()
        .join(", ");
    ResponseData::FrameSwitched {
        success: false,
        frame_id: None,
        name: None,
        url: None,
        error: Some(WebPilotError::InvalidArgument {
            detail: format!(
                "{} frames match {what} — refine it or use `frame predicate` to pick one: {urls}",
                hits.len()
            ),
        }),
    }
}

impl LocalTransport {
    // ── Tabs ─────────────────────────────────────────────────────────────

    pub(super) async fn do_tab_list(&self) -> Result<ResponseData> {
        let targets = self.browser.get_targets().await?;
        let ctx = self.browser_context_id.as_deref();
        let created = self.browser.get_browser_contexts().await?;
        let tabs: Vec<TabInfo> = targets
            .into_iter()
            .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            .filter(|t| target_in_context(t, ctx, &created))
            .map(|t| TabInfo {
                id: t
                    .get("targetId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                url: t
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                title: t
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                // Active = WebPilot's own pinned tab, not CDP `attached`. The
                // `attached` flag is true for ANY debugger client on the target,
                // so an open DevTools window or a second tool would mark a tab
                // (or several) active that the agent never pinned. The pin is
                // `self.target_id`, the one this transport acts on.
                active: t.get("targetId").and_then(|v| v.as_str()) == Some(self.target_id.as_str()),
            })
            .collect();
        Ok(ResponseData::Tabs { tabs })
    }

    /// Typed `TabNotFound` if no page target *in this browser context* carries
    /// the id — guards tab operations so an unknown id surfaces exit 4 instead
    /// of a raw CDP error, and so a context-scoped agent can never reach a tab
    /// belonging to another context.
    async fn ensure_tab_exists(&self, tab_id: &str) -> Result<Option<ResponseData>> {
        let ctx = self.browser_context_id.as_deref();
        let targets = self.browser.get_targets().await?;
        let created = self.browser.get_browser_contexts().await?;
        let exists = targets.iter().any(|t| {
            t.get("targetId").and_then(|v| v.as_str()) == Some(tab_id)
                && t.get("type").and_then(|v| v.as_str()) == Some("page")
                && target_in_context(t, ctx, &created)
        });
        Ok((!exists).then(|| ResponseData::Error {
            error: WebPilotError::TabNotFound {
                tab_id: tab_id.to_string(),
            },
        }))
    }

    /// A failed tab operation may be tab-gone truth in disguise — the target
    /// closed between `ensure_tab_exists` and the CDP call. Re-query the live
    /// targets: absent → typed `TabNotFound` (exit 4 → recover via `tab`),
    /// present → the original error (a genuinely failed operation). The same
    /// split browser mode's catch arms already make.
    async fn tab_gone_or(&self, e: anyhow::Error, tab_id: &str) -> Result<ResponseData> {
        if self.target_absent(tab_id).await {
            return Ok(ResponseData::Error {
                error: WebPilotError::TabNotFound {
                    tab_id: tab_id.to_string(),
                },
            });
        }
        Err(e)
    }

    pub(super) async fn do_tab_switch(
        &mut self,
        tab_id: &str,
        reinstall_now: bool,
    ) -> Result<ResponseData> {
        if let Some(not_found) = self.ensure_tab_exists(tab_id).await? {
            return Ok(not_found);
        }

        if let Err(e) = self
            .browser
            .send("Target.activateTarget", Some(json!({"targetId": tab_id})))
            .await
        {
            return self.tab_gone_or(e, tab_id).await;
        }
        let new_page = match attach_to_page(&self.browser, tab_id).await {
            Ok(p) => p,
            Err(e) => return self.tab_gone_or(e, tab_id).await,
        };
        self.page = new_page;
        self.target_id = tab_id.to_string();
        *self.active_frame_id.lock().await = None;
        super::clear_persisted_active_frame(self.persisted_context_key());
        super::write_persisted_active_tab(self.persisted_context_key(), tab_id)?;
        self.rebind_page_world().await?;
        // Armed monitors follow the agent's working tab. A plain `tab switch` lands
        // on an already-loaded page, so re-arm now. `tab new` and popup adoption
        // pass `false`: their target is still about:blank here and the imminent
        // document load wipes window-level hooks, so they re-arm AFTER the new
        // document settles instead.
        if reinstall_now {
            self.reinstall_monitors().await;
        }
        Ok(action_success(None))
    }

    /// Adopt a tab the acted-on page opened during the action window — the
    /// headless mirror of browser mode's `tabs.onCreated` correlation. The
    /// first buffered `Target.targetCreated` whose `openerId` is the acted-on
    /// target moves the pin, exactly as a user-visible popup steals focus.
    /// Only a tab THIS page opened qualifies; an unrelated target created
    /// concurrently (another context, another agent) never captures the pin.
    pub(super) async fn adopt_click_opened_target(
        &mut self,
        events: &mut tokio::sync::broadcast::Receiver<Value>,
    ) -> Option<TabInfo> {
        use tokio::sync::broadcast::error::TryRecvError;
        let opener = self.target_id.clone();
        let mut lagged = false;
        let info = loop {
            match events.try_recv() {
                Ok(ev) => {
                    if ev.get("method").and_then(Value::as_str) != Some("Target.targetCreated") {
                        continue;
                    }
                    let Some(info) = ev.pointer("/params/targetInfo") else {
                        continue;
                    };
                    if info.get("type").and_then(Value::as_str) == Some("page")
                        && info.get("openerId").and_then(Value::as_str) == Some(opener.as_str())
                    {
                        break info.clone();
                    }
                }
                Err(TryRecvError::Lagged(_)) => {
                    lagged = true;
                    continue;
                }
                Err(_) if lagged => {
                    let targets = self.browser.get_targets().await.ok()?;
                    let mut matches = targets.into_iter().filter(|info| {
                        info.get("type").and_then(Value::as_str) == Some("page")
                            && info.get("openerId").and_then(Value::as_str) == Some(opener.as_str())
                    });
                    let only = matches.next()?;
                    if matches.next().is_some() {
                        return None;
                    }
                    break only;
                }
                Err(_) => return None,
            }
        };
        let id = info.get("targetId").and_then(Value::as_str)?.to_string();
        // Rebind through the same path `tab switch` uses, so frame scope,
        // persistence, and monitors stay consistent. A popup that already
        // vanished is simply not adopted.
        match self.do_tab_switch(&id, false).await {
            Ok(ResponseData::Action { success: true, .. }) => {}
            _ => return None,
        }
        // The creation event carries about:blank and an empty title; the pin has
        // moved, so wait for the popup to commit and parse, then read its settled
        // identity from the live target. A slow or redirecting popup would
        // otherwise describe the tab the agent is now pinned to as a page it has
        // already left. (This settle also lets the auto-capture skip its own.)
        let mut popup_events = self.page.subscribe_events();
        self.await_adopted_document(&mut popup_events).await;
        // Re-arm monitors now that the adopted popup has left about:blank and
        // committed its real document — the early arm in `do_tab_switch` was
        // skipped (`false`) because that load would have wiped it.
        self.reinstall_monitors().await;
        let settled = self
            .browser
            .send("Target.getTargetInfo", Some(json!({ "targetId": id })))
            .await
            .ok();
        let from_settled = |key: &str| -> Option<String> {
            settled
                .as_ref()
                .and_then(|v| v.pointer(&format!("/targetInfo/{key}")))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let url = from_settled("url")
            .filter(|u| !u.is_empty() && u != "about:blank")
            .unwrap_or_else(|| {
                info.get("url")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            });
        let title = from_settled("title").unwrap_or_else(|| {
            info.get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        });
        Some(TabInfo {
            id,
            url,
            title,
            active: true,
        })
    }

    pub(super) async fn do_tab_new(&mut self, url: &str) -> Result<ResponseData> {
        // The tab the agent is bound to now — restored if this `tab new` fails, so
        // a bad URL never strands an orphan tab or silently drifts the pin onto a
        // `chrome-error://` page. This is `navigate`'s no-leak contract: a failed
        // load leaves the agent exactly where it started.
        let prev_target = self.target_id.clone();

        // Open the tab BLANK, then drive the requested load through
        // `navigate_reconnect` — the very path `action navigate` uses.
        // `Target.createTarget(about:blank)` is instant and always lands a stable
        // page, so the switch's existence guards never race a refused-URL
        // error-page transition (the source of an occasional false `TabNotFound`).
        // The load then reuses `navigate`'s fast, typed failure detection
        // (`Page.navigate`'s `errorText` → NavigationFailed at once) instead of a
        // bespoke settle-then-`unreachableUrl` probe that waits the whole
        // navigation timeout on a refused URL. One load-and-failure path for both
        // commands, by construction.
        let target_id = self
            .browser
            .create_target("about:blank", self.browser_context_id.as_deref())
            .await?;
        // A new tab becomes the active one — same UX as `chrome.tabs.create`
        // in browser mode. Rebind through the `tab switch` path so a
        // long-lived transport (the MCP server) acts on the tab it just
        // created, not the page it was bound to before.
        match self.do_tab_switch(&target_id, false).await? {
            ResponseData::Action { success: true, .. } => {}
            other => {
                self.rollback_tab_new(&target_id, &prev_target).await;
                return Ok(other);
            }
        }
        // Load the URL exactly as `navigate` does: a refused/DNS failure is a
        // typed `NavigationFailed` (fast, via `Page.navigate`'s `errorText`), and a
        // failed open rolls back to the agent's previous tab — `navigate`'s no-leak
        // contract, now literally the same code. `navigate_reconnect` settles the
        // load and re-arms monitors on the new document itself.
        if let Err(e) = self.navigate_reconnect(url).await {
            self.rollback_tab_new(&target_id, &prev_target).await;
            return Err(e);
        }
        // `navigate_reconnect` re-arms monitors on every path that BUILDS a new
        // document, but a purely same-document settle (a fragment-only target such
        // as `about:blank#x`) returns without arming — and the `false` switch above
        // deferred the arm. Guarantee it here so `do_tab_new`'s postcondition holds
        // by its own code: the new tab always carries the agent's armed monitors,
        // whichever settle path the load took. Idempotent — guarded on the install
        // flag — so the common (new-document) case pays only a no-op probe.
        self.reinstall_monitors().await;
        let url = self
            .eval_in_active("location.href")
            .await
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| url.to_string());
        let title = self
            .eval_in_active("document.title")
            .await
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        Ok(ResponseData::Action {
            success: true,
            error: None,
            dom: None,
            url_changed: None,
            new_tab: Some(TabInfo {
                id: target_id,
                url,
                title,
                active: true,
            }),
            capture_error: None,
        })
    }

    /// Roll back a failed `tab new`: close the just-created tab and re-pin the
    /// tab the agent was on before it. A failed `tab new` must leave the agent
    /// exactly where it started — `navigate`'s no-leak contract, where a bad URL
    /// never strands a tab or drifts the pin onto a `chrome-error://` page.
    /// Best-effort: the orphan is closed and the previous pin restored; if the
    /// previous tab itself vanished the re-pin can't bind, but the orphan is gone.
    async fn rollback_tab_new(&mut self, orphan: &str, prev: &str) {
        let _ = self
            .browser
            .send(
                "Target.closeTarget",
                Some(serde_json::json!({ "targetId": orphan })),
            )
            .await;
        if prev != orphan {
            // Restoring the agent's previous tab is a plain `tab switch` back onto
            // an already-loaded page — re-arm its monitors now (`true`), not the
            // `false` the forward path used for the about-to-load blank tab. This
            // makes the rollback land the agent exactly where a `tab switch` would,
            // monitors included, even if `prev` was navigated out-of-band meanwhile.
            let _ = self.do_tab_switch(prev, true).await;
        }
    }

    pub(super) async fn do_tab_close(&self, tab_id: &str) -> Result<ResponseData> {
        if let Some(not_found) = self.ensure_tab_exists(tab_id).await? {
            return Ok(not_found);
        }

        // Closing the tab this session is bound to: record it as the active pin
        // FIRST, so the next command sees a DEAD pin — including the fresh-session
        // case where no pin was persisted yet and the active tab is just the
        // implicit `target_id`. `pick_active_target` then drops the dead pin and
        // attaches to a fallback survivor, marking the transport `pin_vanished`:
        // a page ACTION fails loud (`send` → TabNotFound) rather than silently
        // running on the survivor, while `tab` list/switch and `status` proceed so
        // the agent can re-pin. Browser mode gets the same effect from its sticky
        // pin.
        if self.target_id.as_str() == tab_id {
            super::write_persisted_active_tab(self.persisted_context_key(), tab_id)?;
        }
        if let Err(e) = self
            .browser
            .send("Target.closeTarget", Some(json!({"targetId": tab_id})))
            .await
        {
            return self.tab_gone_or(e, tab_id).await;
        }
        Ok(action_success(None))
    }

    // ── Frames ───────────────────────────────────────────────────────────

    pub(super) async fn do_frame_list(&self) -> Result<ResponseData> {
        let result = self.page.send("Page.getFrameTree", None).await?;
        let mut frames = Vec::new();
        if let Some(tree) = result.get("frameTree") {
            collect_frames(tree, 0, &mut frames);
        }
        let mut active_frame_id = self.active_frame_id.lock().await.clone();
        // A persisted active-frame id can outlive its frame — a prior process
        // switched into it, then the page dropped it. `frame list` is the recovery
        // path: reset the stale scope to main and REPORT it (active_frame_id: None)
        // so the agent re-orients explicitly, rather than being silently
        // retargeted. Mirrors browser `handleFrameList`; the open-time restore
        // deliberately keeps a vanished id so a scoped command FrameNotFounds first.
        if let Some(fid) = &active_frame_id
            && !frames.iter().any(|f| &f.frame_id == fid)
        {
            *self.active_frame_id.lock().await = None;
            super::clear_persisted_active_frame(self.persisted_context_key());
            active_frame_id = None;
        }
        Ok(ResponseData::Frames {
            frames,
            active_frame_id,
        })
    }

    pub(super) async fn do_frame_switch(&self, selector: FrameSelector) -> Result<ResponseData> {
        // A predicate's `eval` gate is enforced at the transport boundary
        // (`Command::policy_key`), before this handler runs.
        if matches!(selector, FrameSelector::Main) {
            *self.active_frame_id.lock().await = None;
            super::clear_persisted_active_frame(self.persisted_context_key());
            return Ok(ResponseData::FrameSwitched {
                success: true,
                frame_id: None,
                name: Some("main".into()),
                url: None,
                error: None,
            });
        }

        let tree = self.page.send("Page.getFrameTree", None).await?;
        let mut all = Vec::new();
        if let Some(t) = tree.get("frameTree") {
            collect_frames(t, 0, &mut all);
        }

        // Only HTTP(S) subframes are switch targets — the same scheme filter
        // browser mode applies and the same one `count_http_subframes` uses to
        // surface "N iframe(s) not shown". So the set an agent can switch into
        // matches the set it is told about, and headless and browser agree:
        // a named `about:blank` / `srcdoc` / `data:` / `file:` iframe is not a
        // frame-switch target in either mode.
        let candidates: Vec<&FrameInfo> = all
            .iter()
            .filter(|f| !f.is_main && f.url.starts_with("http"))
            .collect();

        let matched: Option<&FrameInfo> = match &selector {
            FrameSelector::Main => unreachable!("handled above"),
            FrameSelector::Name { value } => {
                let hits: Vec<&FrameInfo> = candidates
                    .iter()
                    .copied()
                    .filter(|f| f.name.as_deref() == Some(value.as_str()))
                    .collect();
                if hits.len() > 1 {
                    // The frame name is page-controlled — sanitize/cap it like the
                    // URL path below, so a hostile name can't spoof or flood this
                    // agent-facing ambiguity error.
                    return Ok(ambiguous_frame_switch(
                        &hits,
                        format!("name \"{}\"", webpilot::types::line_safe_clip(value, 200)),
                    ));
                }
                hits.into_iter().next()
            }
            FrameSelector::Url { pattern } => {
                let hits: Vec<&FrameInfo> = candidates
                    .iter()
                    .copied()
                    .filter(|f| webpilot::url_glob::matches(pattern, &f.url))
                    .collect();
                if hits.len() > 1 {
                    return Ok(ambiguous_frame_switch(
                        &hits,
                        format!("url pattern \"{pattern}\""),
                    ));
                }
                hits.into_iter().next()
            }
            FrameSelector::Predicate { js } => {
                // The predicate rides the SAME form decision as `eval` — compile
                // to detect expression vs statements, then evaluate — so a
                // statement-form predicate (`const ok = …; ok`) behaves the same
                // here as in `eval` and as in browser mode's `cdpEval`.
                let form = self.eval_form(js).await?;
                // Settle every candidate's MAIN-world context before judging
                // them: the async listener can lag a navigation, and a candidate
                // whose context hasn't landed yet would otherwise be skipped and
                // the matching frame missed.
                let candidate_ids: Vec<String> =
                    candidates.iter().map(|f| f.frame_id.clone()).collect();
                self.settle_frame_contexts(&candidate_ids).await;
                // Resolve every candidate's context CONCURRENTLY under one
                // shared PROBE budget. `settle` already waited 500ms best-effort;
                // a still-missing candidate is either a same-process frame
                // churning slower than that budget (live — its context WILL
                // land) or a cross-origin OOPIF with none in this session at
                // all. Waiting per-candidate SERIALLY would pay PROBE (2s) for
                // EACH missing one — a page with many OOPIFs would stall N×2s.
                // Racing them together bounds the whole resolve at one PROBE: a
                // live-but-slow frame is judged rather than silently missed (a
                // false-negative `FrameNotFound`), an OOPIF still times out to a
                // clean skip (treated like a non-match).
                let resolved: Vec<_> =
                    futures_util::future::join_all(candidates.iter().map(|f| async move {
                        self.await_context(&self.frame_contexts, &f.frame_id)
                            .await
                            .ok()
                            .map(|cid| (f, cid))
                    }))
                    .await
                    .into_iter()
                    .flatten()
                    .collect();
                let mut matches: Vec<&_> = Vec::new();
                let mut predicate_error = None;
                // Evaluate the predicate SERIALLY over the resolved contexts —
                // concurrent `Runtime.evaluate`s would contend on the page
                // session; only the context RESOLVE needed to race.
                for (f, cid) in resolved {
                    match self.eval_in_context(&form, Some(&cid), true).await {
                        Ok(v) => {
                            if v.get("value").and_then(|v| v.as_bool()) == Some(true) {
                                matches.push(f);
                            }
                        }
                        // A predicate that THREW (a broken expression) is not a
                        // frame that cleanly evaluated false. Remember the error;
                        // if nothing matches, surface it rather than a misleading
                        // FrameNotFound that implies the predicate ran everywhere.
                        Err(e) => predicate_error = Some(e),
                    }
                }
                // The strict-selector contract (`frame url`, `tab find`,
                // `find --click`): a predicate true in MORE than one frame
                // would silently scope every later command to whichever
                // matched first. Fail loud naming the matching frames.
                if matches.len() > 1 {
                    // Cap each URL like the named-selector ambiguity and the
                    // browser-mode predicate path — a data-URI iframe among the
                    // matches would otherwise flood the message; the full URLs
                    // are in `frame list`'s JSON.
                    let urls: Vec<String> = matches
                        .iter()
                        .map(|f| webpilot::types::line_safe_clip(&f.url, 200))
                        .collect();
                    return Err(WebPilotError::InvalidArgument {
                        detail: format!(
                            "{} frames match the predicate — narrow it (urls: {})",
                            matches.len(),
                            urls.join(", ")
                        ),
                    }
                    .into());
                }
                if matches.is_empty()
                    && let Some(e) = predicate_error
                {
                    return Err(e);
                }
                matches.pop().copied()
            }
        };

        match matched {
            Some(frame) => {
                // The async listener may not have recorded this frame's
                // executionContextId yet — without it, every subsequent
                // `eval`/`invoke_bridge` would silently fall back to the main
                // world. Settle until the map catches up (or the budget expires).
                if !self
                    .frame_contexts
                    .lock()
                    .await
                    .contains_key(&frame.frame_id)
                {
                    self.settle_frame_contexts(std::slice::from_ref(&frame.frame_id))
                        .await;
                }
                // A cross-origin OOPIF has no execution context in this tab's CDP
                // session, so it never resolves: committing the switch would return
                // success, then fail every later eval/capture with FrameNotFound
                // (after a per-command probe). Fail loud AT THE SWITCH instead — the
                // documented OOPIF boundary the predicate path already enforces by
                // dropping unresolved candidates; Url/Name now agree.
                if !self
                    .frame_contexts
                    .lock()
                    .await
                    .contains_key(&frame.frame_id)
                {
                    return Ok(ResponseData::FrameSwitched {
                        success: false,
                        frame_id: None,
                        name: None,
                        url: None,
                        error: Some(WebPilotError::FrameNotFound {
                            selector: serde_json::to_string(&selector)
                                .expect("FrameSelector serializes losslessly"),
                        }),
                    });
                }
                *self.active_frame_id.lock().await = Some(frame.frame_id.clone());
                super::write_persisted_active_frame(self.persisted_context_key(), &frame.frame_id)?;
                Ok(ResponseData::FrameSwitched {
                    success: true,
                    frame_id: Some(frame.frame_id.clone()),
                    name: frame.name.clone(),
                    url: Some(frame.url.clone()),
                    error: None,
                })
            }
            None => {
                let detail =
                    serde_json::to_string(&selector).expect("FrameSelector serializes losslessly");
                Ok(ResponseData::FrameSwitched {
                    success: false,
                    frame_id: None,
                    name: None,
                    url: None,
                    error: Some(WebPilotError::FrameNotFound { selector: detail }),
                })
            }
        }
    }

    // ── Status ───────────────────────────────────────────────────────────

    pub(super) async fn do_status(&self) -> Result<ResponseData> {
        let version = self.browser.send("Browser.getVersion", None).await?;
        let chrome_version = version
            .get("product")
            .and_then(|v| v.as_str())
            .map(parse_chrome_product);
        let title = self
            .page
            .evaluate("document.title")
            .await
            .ok()
            .and_then(|v| v.as_str().map(str::to_string));
        let url = self
            .page
            .evaluate("location.href")
            .await
            .ok()
            .and_then(|v| v.as_str().map(str::to_string));

        Ok(ResponseData::Status {
            connected: true,
            mode: RunMode::Headless,
            tab_url: url,
            tab_title: title,
            chrome_version,
            extension_version: None,
        })
    }
}

// ── Frame-tree walking helper ────────────────────────────────────────────

fn collect_frames(node: &Value, depth: u32, out: &mut Vec<FrameInfo>) {
    if depth > super::MAX_FRAME_DEPTH {
        return;
    }
    if let Some(frame) = node.get("frame") {
        out.push(FrameInfo {
            frame_id: frame
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            url: frame
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            name: frame
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            parent_frame_id: frame
                .get("parentId")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            is_main: frame.get("parentId").is_none(),
        });
    }
    if let Some(children) = node.get("childFrames").and_then(|v| v.as_array()) {
        for child in children {
            collect_frames(child, depth + 1, out);
        }
    }
}

/// Strip the engine prefix from `Browser.getVersion`'s `product` field so
/// the resulting `chrome_version` matches the format produced by the
/// browser-mode service worker (which extracts just the version digits from
/// `navigator.userAgent`). Recognises both `Chrome/...` and Chrome's old
/// `HeadlessChrome/...` shape.
fn parse_chrome_product(product: &str) -> String {
    product
        .strip_prefix("HeadlessChrome/")
        .or_else(|| product.strip_prefix("Chrome/"))
        .unwrap_or(product)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_glob_matches_substring_and_wildcards() {
        // The matcher itself is unit-tested in `webpilot::url_glob`; this guards
        // that `frame url` routes through it (not the old star-stripping).
        assert!(webpilot::url_glob::matches(
            "auth*login",
            "https://x/auth/x/login"
        ));
        assert!(!webpilot::url_glob::matches(
            "login*auth",
            "https://x/auth/login"
        ));
    }

    #[test]
    fn parse_chrome_product_strips_chrome_prefix() {
        assert_eq!(
            parse_chrome_product("Chrome/120.0.6099.71"),
            "120.0.6099.71"
        );
    }

    #[test]
    fn parse_chrome_product_strips_headless_chrome_prefix() {
        assert_eq!(
            parse_chrome_product("HeadlessChrome/120.0.6099.71"),
            "120.0.6099.71"
        );
    }

    #[test]
    fn parse_chrome_product_passes_unknown_prefix_through() {
        assert_eq!(parse_chrome_product("Brave/1.2.3"), "Brave/1.2.3");
    }
}
