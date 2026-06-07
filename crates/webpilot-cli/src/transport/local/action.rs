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
                });
            }
            Action::Back => {
                self.history_nav("history.back()").await?;
                return Ok(action_success(None));
            }
            Action::Forward => {
                self.history_nav("history.forward()").await?;
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

        let action_json = serde_json::to_value(&action)?;
        let raw = self
            .invoke_bridge(&json!({"type": "executeAction", "action": action_json}))
            .await?;
        let _ = Self::parse_bridge_response(raw)?;

        let dom = if capture {
            let r = self
                .invoke_bridge(&json!({"type": "extractDom", "options": {}}))
                .await?;
            let mut snapshot: Option<webpilot::types::DomSnapshot> = serde_json::from_value(r).ok();
            if let Some(s) = snapshot.as_mut()
                && self.active_frame_id.lock().await.is_none()
            {
                s.subframes = self.count_http_subframes().await;
            }
            snapshot
        } else {
            None
        };

        Ok(action_success(dom))
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

    /// Drive a same-document history navigation (`history.back()`/`forward()`).
    ///
    /// The navigation tears down the execution context the expression runs in,
    /// so the evaluate's own response can come back as a CDP "target navigated"
    /// teardown — that is the success path, not a failure. Only typed transport
    /// failures (`ConnectionLost`/`Timeout`) are propagated; the navigation is
    /// then confirmed via the frame event and the active frame is reset, just
    /// as `navigate_reconnect` does.
    async fn history_nav(&self, expression: &str) -> Result<()> {
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
