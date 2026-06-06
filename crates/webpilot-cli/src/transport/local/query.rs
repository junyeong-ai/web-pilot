//! Page query operations: evaluate, wait, dom get/set, fetch.

use anyhow::Result;
use serde_json::json;
use webpilot::WebPilotError;
use webpilot::protocol::{DomProperty, ResponseData};
use webpilot::types::PolicyKey;
use webpilot::wait::WaitCondition;

use super::LocalTransport;
use super::state::policy_store;

/// Recognise V8's SyntaxError variants so we know when to retry as a
/// multi-statement script. `cdp::evaluate` surfaces the exception's
/// `description` text, which always starts with "SyntaxError:".
fn is_syntax_error(e: &anyhow::Error) -> bool {
    e.to_string().starts_with("SyntaxError")
}

impl LocalTransport {
    pub(super) async fn do_evaluate(&self, code: &str) -> Result<ResponseData> {
        if policy_store::denies(PolicyKey::Eval) {
            return Ok(ResponseData::Eval {
                success: false,
                result: None,
                error: Some(WebPilotError::PolicyDenied {
                    operation: PolicyKey::Eval.to_string(),
                }),
            });
        }
        // First try as a single expression (so `{a: 1}` is read as an object
        // literal, not a labeled statement). Fall back to multi-statement
        // form on `SyntaxError` so things like `console.log(x); x` still work.
        let expression_form = format!("(()=>({code}))()");
        let attempt = self.eval_in_active(&expression_form).await;
        let val = match attempt {
            Ok(v) => Ok(v),
            Err(e) if is_syntax_error(&e) => self.eval_in_active(code).await,
            Err(e) => Err(e),
        };
        match val {
            Ok(v) => Ok(ResponseData::Eval {
                success: true,
                result: Some(serde_json::to_string(&v).unwrap_or_else(|_| "null".into())),
                error: None,
            }),
            Err(e) => Ok(ResponseData::Eval {
                success: false,
                result: None,
                error: Some(WebPilotError::Other {
                    detail: e.to_string(),
                }),
            }),
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
        if policy_store::denies(PolicyKey::Fetch) {
            return Ok(ResponseData::FetchResult {
                success: false,
                status: None,
                body: None,
                error: Some(WebPilotError::PolicyDenied {
                    operation: PolicyKey::Fetch.to_string(),
                }),
            });
        }
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
