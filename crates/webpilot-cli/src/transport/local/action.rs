//! Page-mutating actions: dispatch, drag, navigation passthrough.
//!
//! All bridge-routed actions forward the typed `Action` directly to
//! `bridge.js`'s `executeAction` handler; CDP-level actions (navigate, drag,
//! history, reload) are handled inline against the page CDP connection.

use anyhow::Result;
use serde_json::json;
use webpilot::protocol::ResponseData;
use webpilot::types::PolicyVerdict;
use webpilot::{Action, WebPilotError};

use super::state::policy_store;
use super::{LocalTransport, action_success};

impl LocalTransport {
    pub(super) async fn do_action(
        &mut self,
        action: Action,
        capture: bool,
    ) -> Result<ResponseData> {
        // Policy enforcement — same wire format as browser-mode SW so a single
        // `webpilot policy set --action click --verdict deny` is honoured in
        // both modes.
        let kind = action.kind();
        if policy_store::read().get(&kind) == Some(&PolicyVerdict::Deny) {
            return Ok(ResponseData::Action {
                success: false,
                error: Some(WebPilotError::PolicyDenied {
                    action: kind.to_string(),
                }),
                dom: None,
                url_changed: None,
                new_tab: None,
            });
        }

        match &action {
            Action::Navigate { url } => {
                let url = url.clone();
                self.navigate_reconnect(&url).await?;
                return Ok(action_success(None));
            }
            Action::Back => {
                self.page.evaluate("history.back()").await?;
                self.page
                    .wait_for_event("Page.frameNavigated", crate::timeouts::back_forward())
                    .await
                    .ok();
                return Ok(action_success(None));
            }
            Action::Forward => {
                self.page.evaluate("history.forward()").await?;
                self.page
                    .wait_for_event("Page.frameNavigated", crate::timeouts::back_forward())
                    .await
                    .ok();
                return Ok(action_success(None));
            }
            Action::Reload => {
                self.page.send("Page.reload", None).await?;
                self.page
                    .wait_for_event("Page.loadEventFired", crate::timeouts::reload_wait())
                    .await
                    .ok();
                return Ok(action_success(None));
            }
            Action::Drag {
                source,
                target,
                steps,
            } => {
                self.do_drag(*source, *target, *steps).await?;
                return Ok(action_success(None));
            }
            Action::Hover { index } => {
                // Browser-input mouse move so CSS `:hover` actually fires.
                // Bridge.js dispatchEvent only triggers JS listeners, not the
                // internal hover state.
                self.do_hover(*index).await?;
                return Ok(action_success(None));
            }
            Action::Upload { index, path } => {
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
                .invoke_bridge(&json!({"type": "extractDOM", "options": {}}))
                .await?;
            serde_json::from_value(r).ok()
        } else {
            None
        };

        Ok(action_success(dom))
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

        self.page
            .send(
                "DOM.setFileInputFiles",
                Some(json!({
                    "nodeId": node_id,
                    "files": [path.to_string_lossy()],
                })),
            )
            .await?;

        let _ = self
            .invoke_bridge(&json!({"type": "untagElement", "attr": "data-wp-upload"}))
            .await;
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
        let x = resp["sx"].as_f64().unwrap_or(0.0);
        let y = resp["sy"].as_f64().unwrap_or(0.0);
        self.page
            .send(
                "Input.dispatchMouseEvent",
                Some(json!({"type": "mouseMoved", "x": x, "y": y, "button": "none"})),
            )
            .await?;
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

        let sx = resp["sx"].as_f64().unwrap_or(0.0);
        let sy = resp["sy"].as_f64().unwrap_or(0.0);
        let tx = resp["tx"].as_f64().unwrap_or(0.0);
        let ty = resp["ty"].as_f64().unwrap_or(0.0);

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
