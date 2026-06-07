//! Page query operations: evaluate, wait, dom get/set, fetch.

use anyhow::Result;
use serde_json::json;
use webpilot::WebPilotError;
use webpilot::protocol::{DomProperty, ResponseData};
use webpilot::wait::WaitCondition;

use super::LocalTransport;

impl LocalTransport {
    /// Whether `code` compiles as a single expression. Decided by COMPILING —
    /// never by executing and retrying: a runtime `throw new SyntaxError(...)`
    /// thrown from a perfectly valid expression must not trigger a second run
    /// of the code (duplicated side effects), and error-message inspection is
    /// banned project-wide. `compileScript` with `persistScript: false`
    /// parses without evaluating anything.
    async fn parses_as_expression(&self, code: &str) -> Result<bool> {
        let mut params = json!({
            "expression": format!("({code})"),
            "sourceURL": "webpilot://eval-form-probe",
            "persistScript": false,
        });
        if let Some(cid) = self.active_context_id().await? {
            params["executionContextId"] = cid.into();
        }
        let r = self.page.send("Runtime.compileScript", Some(params)).await?;
        Ok(r.get("exceptionDetails").is_none())
    }

    pub(super) async fn do_eval(&self, code: &str) -> Result<ResponseData> {
        // Prefer the expression form whenever the code COMPILES as one (so
        // `{a: 1}` is read as an object literal, not a labeled statement);
        // everything else runs as a multi-statement script.
        let val = if self.parses_as_expression(code).await? {
            self.eval_in_active(&format!("(()=>({code}))()")).await
        } else {
            self.eval_in_active(code).await
        };
        match val {
            Ok(v) => Ok(ResponseData::Eval {
                success: true,
                result: Some(serde_json::to_string(&v).unwrap_or_else(|_| "null".into())),
                error: None,
            }),
            Err(e) => {
                // Keep a typed error typed (e.g. FrameNotFound from a vanished
                // switched frame → exit 4); only wrap genuinely unknown ones.
                let error = match e.downcast::<WebPilotError>() {
                    Ok(typed) => typed,
                    Err(e) => WebPilotError::Other {
                        detail: e.to_string(),
                    },
                };
                Ok(ResponseData::Eval {
                    success: false,
                    result: None,
                    error: Some(error),
                })
            }
        }
    }

    pub(super) async fn do_wait(
        &self,
        condition: WaitCondition,
        timeout_ms: u64,
    ) -> Result<ResponseData> {
        if matches!(condition, WaitCondition::Navigation) {
            return match self
                .page
                .wait_for_event(
                    "Page.loadEventFired",
                    std::time::Duration::from_millis(timeout_ms),
                )
                .await
            {
                Ok(_) => Ok(ResponseData::Wait {
                    success: true,
                    error: None,
                }),
                Err(_) => Ok(ResponseData::Wait {
                    success: false,
                    error: Some(WebPilotError::Timeout {
                        kind: "navigation".into(),
                        elapsed_ms: timeout_ms,
                    }),
                }),
            };
        }

        let raw = self
            .invoke_bridge(&json!({
                "type": "wait",
                "condition": condition,
                "timeout_ms": timeout_ms,
            }))
            .await?;

        match Self::parse_bridge_response(raw) {
            Ok(_) => Ok(ResponseData::Wait {
                success: true,
                error: None,
            }),
            Err(e) => {
                let err = e
                    .downcast_ref::<WebPilotError>()
                    .cloned()
                    .unwrap_or_else(|| WebPilotError::Other {
                        detail: e.to_string(),
                    });
                Ok(ResponseData::Wait {
                    success: false,
                    error: Some(err),
                })
            }
        }
    }

    pub(super) async fn do_dom_get(
        &self,
        selector: &str,
        property: DomProperty,
    ) -> Result<ResponseData> {
        let msg = match property {
            DomProperty::Html => json!({"type": "getHtml", "selector": selector}),
            DomProperty::Text => json!({"type": "getText", "selector": selector}),
            DomProperty::Attr { name } => {
                json!({"type": "getAttr", "selector": selector, "attr": name})
            }
        };
        let raw = self.invoke_bridge(&msg).await?;
        let data = Self::parse_bridge_response(raw)?;
        Ok(ResponseData::CommandResult {
            success: true,
            value: data
                .get("value")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            error: None,
        })
    }

    pub(super) async fn do_dom_set(
        &self,
        selector: &str,
        property: DomProperty,
        value: &str,
    ) -> Result<ResponseData> {
        let msg = match property {
            DomProperty::Html => json!({"type": "setHtml", "selector": selector, "value": value}),
            DomProperty::Text => json!({"type": "setText", "selector": selector, "value": value}),
            DomProperty::Attr { name } => {
                json!({"type": "setAttr", "selector": selector, "attr": name, "value": value})
            }
        };
        let raw = self.invoke_bridge(&msg).await?;
        let _ = Self::parse_bridge_response(raw)?;
        Ok(ResponseData::CommandResult {
            success: true,
            value: None,
            error: None,
        })
    }

    pub(super) async fn do_fetch(
        &self,
        url: &str,
        method: Option<&str>,
        body: Option<&str>,
    ) -> Result<ResponseData> {
        let body_part = match body {
            Some(b) => format!("body: {},", serde_json::to_string(b)?),
            None => String::new(),
        };
        let js = format!(
            r#"fetch({url}, {{method: {method}, credentials: "include", headers: {{"Content-Type": "application/json"}}, {body_part}}}).then(r => r.text().then(body => ({{status: r.status, body}})))"#,
            url = serde_json::to_string(url)?,
            method = serde_json::to_string(method.unwrap_or("GET"))?,
        );
        let result = self.page.evaluate(&js).await?;
        let status = result
            .get("status")
            .and_then(|v| v.as_u64())
            .map(|s| s as u32);
        let body = result
            .get("body")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        Ok(ResponseData::FetchResult {
            success: true,
            status,
            body,
            error: None,
        })
    }
}
