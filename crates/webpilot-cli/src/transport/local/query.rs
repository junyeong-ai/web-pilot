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

/// The condition rendered for a wait Timeout's `kind` — self-contained
/// (`wait selector "#x"`, never a bare "wait"), mirroring the bridge's own
/// timeout wording so the deadline-race edge in `do_wait` reads identically to
/// a normal in-page expiry. `{:?}` quotes and escapes the agent-supplied value,
/// keeping the message one line.
fn wait_kind(condition: &WaitCondition) -> String {
    match condition {
        WaitCondition::Selector { value } => format!("wait selector {value:?}"),
        WaitCondition::Text { value } => format!("wait text {value:?}"),
        WaitCondition::Navigation => "wait navigation".into(),
        WaitCondition::Idle => "wait idle".into(),
    }
}

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
                // `v` is a `serde_json::Value`, which always serializes — a silent
                // `"null"` fallback would mask an impossible failure, so `expect`
                // it loud (the project rule: never `unwrap_or_default` a
                // serialization).
                result: Some(serde_json::to_string(&v).expect("a JSON value serializes")),
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

    /// Reclassify `err` as the root-cause `TabNotFound` when the page's TARGET
    /// no longer exists: a socket that died (ConnectionLost) or a frame probe
    /// that found nothing (FrameNotFound) because the tab itself closed is
    /// tab-gone truth, not infra or frame scope — the browser client outlives
    /// the page and can say which. The original error is kept for a live tab
    /// (a genuinely dead Chrome stays ConnectionLost; a removed iframe on a
    /// living tab stays FrameNotFound). The browser-mode twin is
    /// `frameVanishedError`'s tab-first split.
    async fn reclassify_if_tab_gone(&self, err: WebPilotError) -> WebPilotError {
        if matches!(
            err,
            WebPilotError::ConnectionLost { .. } | WebPilotError::FrameNotFound { .. }
        ) && self.target_absent(self.target_id.as_str()).await
        {
            return WebPilotError::TabNotFound {
                tab_id: self.target_id.clone(),
            };
        }
        err
    }

    pub(super) async fn do_wait(
        &self,
        condition: WaitCondition,
        timeout_ms: u64,
    ) -> Result<ResponseData> {
        // Clamp at the in-page timer ceiling: `setTimeout`/`setInterval` in the
        // bridge silently overflow past `i32::MAX` ms (~24.8 days) and fire
        // immediately, and `Instant::now() + Duration::from_millis(u64::MAX)`
        // PANICS on overflow (a client-reachable process kill via a pathological
        // `timeout_ms`). One clamp at the entry fixes both — no realistic wait
        // approaches 24 days, so it only ever bounds a degenerate value.
        let timeout_ms = timeout_ms.min(i32::MAX as u64);
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
                Err(e) => {
                    // A typed error from the wait — a dropped CDP socket
                    // (ConnectionLost) or an inconclusive event-buffer overflow
                    // (a Timeout carrying the loss) — is preserved as itself.
                    // Only the untyped deadline expiry collapses to a generic
                    // navigation Timeout; mapping every error there would have
                    // told the agent navigation merely didn't finish when in
                    // fact the connection had died.
                    let err =
                        e.downcast::<WebPilotError>()
                            .unwrap_or_else(|_| WebPilotError::Timeout {
                                kind: "navigation".into(),
                                elapsed_ms: timeout_ms,
                            });
                    Ok(ResponseData::Wait {
                        success: false,
                        error: Some(self.reclassify_if_tab_gone(err).await),
                    })
                }
            };
        }

        // The bridge runs the poll loop in-page and resolves only at its own
        // `timeout_ms`; give the CDP round-trip that long plus the normal
        // `cdp_send` slack, so a long wait isn't truncated to a false Timeout.
        //
        // A document navigation mid-poll destroys the bridge context and fails
        // the in-flight evaluate with the typed `ContextDestroyedMidFlight` —
        // which does not invalidate the wait's intent: the condition may well
        // be satisfied by the NEW document (a redirect landing on the page the
        // agent is waiting for). Re-arm against it with the REMAINING budget
        // (browser mode re-arms identically; Playwright's selector waits
        // survive navigations the same way) instead of surfacing an untyped
        // infra error. A frame REMOVED mid-wait ends differently: its bridge
        // context never reappears, so the re-arm's `bridge_context_id` is a
        // typed FrameNotFound.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let raw = match self
                .invoke_bridge_with_timeout(
                    &json!({
                        "type": "wait",
                        "condition": &condition,
                        "timeout_ms": remaining.as_millis() as u64,
                    }),
                    remaining.saturating_add(webpilot::settings::timeouts().cdp_send),
                )
                .await
            {
                Ok(raw) => raw,
                Err(e)
                    if e.downcast_ref::<crate::cdp::ContextDestroyedMidFlight>()
                        .is_some() =>
                {
                    if std::time::Instant::now() < deadline {
                        continue;
                    }
                    // Destroyed exactly as the budget ran out: the condition
                    // went unsatisfied within the ask — a Timeout, as the
                    // in-page timer would have reported, never an infra error.
                    return Ok(ResponseData::Wait {
                        success: false,
                        error: Some(WebPilotError::Timeout {
                            kind: wait_kind(&condition),
                            elapsed_ms: timeout_ms,
                        }),
                    });
                }
                Err(e) => {
                    // The poll's failure may be tab-gone truth in disguise:
                    // the socket died, or the re-arm's frame probe found
                    // nothing, because the TAB itself closed mid-wait.
                    // Classify like the navigation arm (exit 4 → recover via
                    // `tab`), keeping the original error for a live tab —
                    // a FrameNotFound here would send the agent recapturing
                    // frames on a tab that no longer exists.
                    let err = match e.downcast::<WebPilotError>() {
                        Ok(typed) => self.reclassify_if_tab_gone(typed).await,
                        Err(e) => return Err(e),
                    };
                    return Ok(ResponseData::Wait {
                        success: false,
                        error: Some(err),
                    });
                }
            };

            return match Self::parse_bridge_response(raw) {
                Ok(_) => Ok(ResponseData::Wait {
                    success: true,
                    error: None,
                }),
                Err(e) => {
                    let mut err = e
                        .downcast_ref::<WebPilotError>()
                        .cloned()
                        .unwrap_or_else(|| WebPilotError::Other {
                            detail: e.to_string(),
                        });
                    // The bridge's per-round timer knows only its own
                    // (post-re-arm) residual budget; the agent asked for
                    // `timeout_ms` total. Report the full ask.
                    if let WebPilotError::Timeout { elapsed_ms, .. } = &mut err {
                        *elapsed_ms = timeout_ms;
                    }
                    Ok(ResponseData::Wait {
                        success: false,
                        error: Some(err),
                    })
                }
            };
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
        headers: &[(String, String)],
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
                const r = await fetch({url}, {{method: {method}, credentials: "include", headers: {headers}, {body_part}}});
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
                let body;
                try {{ body = new TextDecoder("utf-8", {{ fatal: true }}).decode(merged); }}
                catch (e) {{ return {{ status: r.status, binary: total }}; }}
                return {{ status: r.status, body }};
            }})()"#,
            url = serde_json::to_string(url)?,
            method = serde_json::to_string(method.unwrap_or("GET"))?,
            // `fetch` accepts an array of `[name, value]` pairs as the headers
            // init directly, so the typed wire shape passes through unchanged.
            headers = serde_json::to_string(headers)?,
            max = FETCH_MAX_BODY_BYTES,
        );
        let result = self.page.evaluate(&js).await?;
        if let Some(max) = result.get("oversize").and_then(|v| v.as_u64()) {
            return Err(WebPilotError::Other {
                detail: format!("response body exceeds the {max}-byte fetch limit"),
            }
            .into());
        }
        // A body that isn't valid UTF-8 is binary; lossy-decoding it would hand
        // the agent mojibake under a success status. Fail loud with the byte
        // count, mirroring the oversize guard above.
        if let Some(bytes) = result.get("binary").and_then(|v| v.as_u64()) {
            return Err(WebPilotError::Other {
                detail: format!("response body is not valid UTF-8 ({bytes} bytes); fetch returns text, not binary"),
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
