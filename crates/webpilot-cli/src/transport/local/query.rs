//! Page query operations: evaluate, wait, dom get/set, fetch.

use anyhow::Result;
use serde_json::json;
use webpilot::WebPilotError;
use webpilot::protocol::{DomProperty, ResponseData};
use webpilot::wait::WaitCondition;

use super::LocalTransport;

/// Cap on the response body `fetch` reads into memory, in both modes. A safety
/// ceiling, not an operational tunable: an API response never approaches it,
/// and a body that does is failed loud rather than read unbounded.
const FETCH_MAX_BODY_BYTES: u64 = 10 * 1024 * 1024;

impl LocalTransport {
    /// Whether `code` compiles as a single expression. Decided by COMPILING —
    /// never by executing and retrying: a runtime `throw new SyntaxError(...)`
    /// thrown from a perfectly valid expression must not trigger a second run
    /// of the code (duplicated side effects), and error-message inspection is
    /// banned project-wide. `compileScript` with `persistScript: false`
    /// parses without evaluating anything.
    async fn parses_as_expression(&self, code: &str) -> Result<bool> {
        // Pure syntax probe: whether `(code)` parses as an expression is the
        // same in every execution context, so it runs in the default one
        // (`compileScript` takes only the reusable integer `executionContextId`,
        // not a `uniqueContextId`, and the frame context is irrelevant to a
        // parse anyway). The actual evaluation still targets the active frame.
        let params = json!({
            "expression": format!("({code})"),
            "sourceURL": "webpilot://eval-form-probe",
            "persistScript": false,
        });
        let r = self
            .page
            .send("Runtime.compileScript", Some(params))
            .await?;
        Ok(r.get("exceptionDetails").is_none())
    }

    /// The evaluable form of `code`: the IIFE-wrapped expression when it
    /// compiles as one (so `{a:1}` is an object literal and the value is
    /// returned), else the statements verbatim. The single decision every eval
    /// path shares — `eval`, `frame find` predicates, and both modes' bridge —
    /// so a statement-form predicate behaves identically everywhere.
    pub(super) async fn eval_form(&self, code: &str) -> Result<String> {
        Ok(if self.parses_as_expression(code).await? {
            format!("(()=>({code}))()")
        } else {
            code.to_string()
        })
    }

    pub(super) async fn do_eval(&self, code: &str) -> Result<ResponseData> {
        // Prefer the expression form whenever the code COMPILES as one (so
        // `{a: 1}` is read as an object literal, not a labeled statement);
        // everything else runs as a multi-statement script.
        let val = self.eval_in_active(&self.eval_form(code).await?).await;
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

        // The bridge runs the poll loop in-page and resolves only at its own
        // `timeout_ms`; give the CDP round-trip that long plus the normal
        // `cdp_send` slack, so a long wait isn't truncated to a false Timeout.
        let cdp_timeout = std::time::Duration::from_millis(timeout_ms)
            .saturating_add(webpilot::settings::timeouts().cdp_send);
        let raw = self
            .invoke_bridge_with_timeout(
                &json!({
                    "type": "wait",
                    "condition": condition,
                    "timeout_ms": timeout_ms,
                }),
                cdp_timeout,
            )
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
        // Stream the response and fail loud past the cap rather than reading an
        // unbounded body into the renderer and back over CDP — a silently
        // truncated body returned as success would be a lie. Mirrors the
        // browser-mode `fetchExpression`.
        let js = format!(
            r#"(async () => {{
                const r = await fetch({url}, {{method: {method}, credentials: "include", headers: {{"Content-Type": "application/json"}}, {body_part}}});
                const MAX = {max};
                const reader = r.body && r.body.getReader();
                if (!reader) return {{ status: r.status, body: "" }};
                const parts = []; let total = 0;
                for (;;) {{
                    const {{ done, value }} = await reader.read();
                    if (done) break;
                    total += value.length;
                    if (total > MAX) {{ try {{ await reader.cancel(); }} catch (e) {{}} return {{ status: r.status, oversize: MAX }}; }}
                    parts.push(value);
                }}
                const merged = new Uint8Array(total); let off = 0;
                for (const p of parts) {{ merged.set(p, off); off += p.length; }}
                return {{ status: r.status, body: new TextDecoder().decode(merged) }};
            }})()"#,
            url = serde_json::to_string(url)?,
            method = serde_json::to_string(method.unwrap_or("GET"))?,
            max = FETCH_MAX_BODY_BYTES,
        );
        let result = self.page.evaluate(&js).await?;
        if let Some(max) = result.get("oversize").and_then(|v| v.as_u64()) {
            return Err(WebPilotError::Other {
                detail: format!("response body exceeds the {max}-byte fetch limit"),
            }
            .into());
        }
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
