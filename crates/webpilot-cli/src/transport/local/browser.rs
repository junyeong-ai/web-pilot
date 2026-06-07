//! Browser-level commands: tabs, frames, status. These operate on the
//! browser-wide CDP target (or query the page's frame tree).

use anyhow::Result;
use serde_json::{Value, json};
use webpilot::WebPilotError;
use webpilot::protocol::{FrameSelector, ResponseData, RunMode};
use webpilot::types::{FrameInfo, TabInfo};

use super::{LocalTransport, action_success, connect_to_page};

impl LocalTransport {
    // ── Tabs ─────────────────────────────────────────────────────────────

    pub(super) async fn do_tab_list(&self) -> Result<ResponseData> {
        let targets = self.browser.get_targets().await?;
        let ctx = self.browser_context_id.as_deref();
        let tabs: Vec<TabInfo> = targets
            .into_iter()
            .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            .filter(|t| match ctx {
                Some(id) => t.get("browserContextId").and_then(|v| v.as_str()) == Some(id),
                None => true,
            })
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
                active: t.get("attached").and_then(|v| v.as_bool()).unwrap_or(false),
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
        let exists = targets.iter().any(|t| {
            t.get("targetId").and_then(|v| v.as_str()) == Some(tab_id)
                && t.get("type").and_then(|v| v.as_str()) == Some("page")
                && match ctx {
                    Some(id) => t.get("browserContextId").and_then(|v| v.as_str()) == Some(id),
                    None => true,
                }
        });
        Ok((!exists).then(|| ResponseData::Error {
            error: WebPilotError::TabNotFound {
                tab_id: tab_id.to_string(),
            },
        }))
    }

    pub(super) async fn do_tab_switch(&mut self, tab_id: &str) -> Result<ResponseData> {
        if let Some(not_found) = self.ensure_tab_exists(tab_id).await? {
            return Ok(not_found);
        }

        self.browser
            .send("Target.activateTarget", Some(json!({"targetId": tab_id})))
            .await?;
        let new_page = connect_to_page(&self.ws_url, tab_id).await?;
        self.page = new_page;
        self.target_id = tab_id.to_string();
        *self.active_frame_id.lock().await = None;
        super::clear_persisted_active_frame(self.persisted_context_key());
        super::write_persisted_active_tab(self.persisted_context_key(), tab_id);
        self.rebind_page_world().await?;
        // Armed monitors follow the agent's working tab: the freshly bound
        // page has no hooks yet (idempotent no-op when nothing is armed).
        self.reinstall_monitors().await;
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
                Err(TryRecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        };
        let id = info.get("targetId").and_then(Value::as_str)?.to_string();
        // Rebind through the same path `tab switch` uses, so frame scope,
        // persistence, and monitors stay consistent. A popup that already
        // vanished is simply not adopted.
        match self.do_tab_switch(&id).await {
            Ok(ResponseData::Action { success: true, .. }) => {}
            _ => return None,
        }
        // The creation event usually carries `about:blank`; the pin has moved,
        // so the bound target reports where the popup actually is by now.
        let url = match self.bound_target_url().await {
            u if u.is_empty() => info
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            u => u,
        };
        Some(TabInfo {
            id,
            url,
            title: info
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            active: true,
        })
    }

    pub(super) async fn do_tab_new(&mut self, url: &str) -> Result<ResponseData> {
        let target_id = match self.browser_context_id.as_deref() {
            Some(ctx) => self.browser.create_target_in_context(ctx, url).await?,
            None => {
                let r = self
                    .browser
                    .send("Target.createTarget", Some(json!({"url": url})))
                    .await?;
                r.get("targetId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            }
        };
        // A new tab becomes the active one — same UX as `chrome.tabs.create`
        // in browser mode. Rebind through the `tab switch` path so a
        // long-lived transport (the MCP server) acts on the tab it just
        // created, not the page it was bound to before.
        match self.do_tab_switch(&target_id).await? {
            ResponseData::Action { success: true, .. } => {}
            other => return Ok(other),
        }
        Ok(ResponseData::Action {
            success: true,
            error: None,
            dom: None,
            url_changed: None,
            new_tab: Some(TabInfo {
                id: target_id,
                url: url.to_string(),
                title: String::new(),
                active: true,
            }),
            capture_error: None,
        })
    }

    pub(super) async fn do_tab_close(&self, tab_id: &str) -> Result<ResponseData> {
        if let Some(not_found) = self.ensure_tab_exists(tab_id).await? {
            return Ok(not_found);
        }

        self.browser
            .send("Target.closeTarget", Some(json!({"targetId": tab_id})))
            .await?;
        if super::read_persisted_active_tab(self.persisted_context_key()).as_deref() == Some(tab_id)
        {
            super::clear_persisted_active_tab(self.persisted_context_key());
            super::clear_persisted_active_frame(self.persisted_context_key());
        }
        Ok(action_success(None))
    }

    // ── Frames ───────────────────────────────────────────────────────────

    pub(super) async fn do_frame_list(&self) -> Result<ResponseData> {
        let result = self.page.send("Page.getFrameTree", None).await?;
        let mut frames = Vec::new();
        if let Some(tree) = result.get("frameTree") {
            collect_frames(tree, &mut frames);
        }
        let active_frame_id = self.active_frame_id.lock().await.clone();
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
            collect_frame_records(t, &mut all);
        }

        let candidates: Vec<&FrameRecord> = all.iter().filter(|f| !f.is_main).collect();

        let matched: Option<&FrameRecord> = match &selector {
            FrameSelector::Main => unreachable!("handled above"),
            FrameSelector::Name { value } => candidates
                .iter()
                .find(|f| f.name.as_deref() == Some(value.as_str()))
                .or_else(|| candidates.iter().find(|f| f.url.contains(value)))
                .copied(),
            FrameSelector::Url { pattern } => {
                let needle = pattern.replace('*', "");
                candidates.iter().find(|f| f.url.contains(&needle)).copied()
            }
            FrameSelector::Predicate { js } => {
                // The predicate rides the SAME form decision as `eval` — compile
                // to detect expression vs statements, then evaluate — so a
                // statement-form predicate (`const ok = …; ok`) behaves the same
                // here as in `eval` and as in browser mode's `cdpEval`.
                let form = self.eval_form(js).await?;
                let mut found = None;
                for f in &candidates {
                    let Some(cid) = self.frame_contexts.lock().await.get(&f.frame_id).cloned()
                    else {
                        continue;
                    };
                    let truthy = self
                        .eval_in_context(&form, Some(&cid), true)
                        .await
                        .ok()
                        .and_then(|v| v.get("value").and_then(|v| v.as_bool()))
                        == Some(true);
                    if truthy {
                        found = Some(*f);
                        break;
                    }
                }
                found
            }
        };

        match matched {
            Some(frame) => {
                // The async listener may not have recorded this frame's
                // executionContextId yet — without it, every subsequent
                // `eval`/`invoke_bridge` would silently fall back to the
                // main world. Force a re-emit and settle until the map
                // catches up (or the budget expires).
                if !self
                    .frame_contexts
                    .lock()
                    .await
                    .contains_key(&frame.frame_id)
                {
                    let _ = self.page.send("Runtime.disable", None).await;
                    let _ = self.page.send("Runtime.enable", None).await;
                    for _ in 0..20 {
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                        if self
                            .frame_contexts
                            .lock()
                            .await
                            .contains_key(&frame.frame_id)
                        {
                            break;
                        }
                    }
                }
                *self.active_frame_id.lock().await = Some(frame.frame_id.clone());
                super::write_persisted_active_frame(self.persisted_context_key(), &frame.frame_id);
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

// ── Frame-tree walking helpers ───────────────────────────────────────────

#[derive(Debug, Clone)]
struct FrameRecord {
    frame_id: String,
    url: String,
    name: Option<String>,
    is_main: bool,
}

fn collect_frame_records(node: &Value, out: &mut Vec<FrameRecord>) {
    if let Some(frame) = node.get("frame") {
        out.push(FrameRecord {
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
            is_main: frame.get("parentId").is_none(),
        });
    }
    if let Some(children) = node.get("childFrames").and_then(|v| v.as_array()) {
        for child in children {
            collect_frame_records(child, out);
        }
    }
}

fn collect_frames(node: &Value, out: &mut Vec<FrameInfo>) {
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
            collect_frames(child, out);
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
