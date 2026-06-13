//! State-keeping commands: cookies, console + network monitoring,
//! session export/import.

use anyhow::Result;
use serde_json::{Value, json};
use webpilot::WebPilotError;
use webpilot::protocol::ResponseData;
use webpilot::types::{
    ConsoleEntry, ConsoleLevel, CookieInfo, NetworkEntry, PartitionKey, SameSite,
};

use super::{LocalTransport, epoch_ms};

/// Schema version stamped into a session export and enforced on import: a file
/// claiming a higher version may carry fields this binary can't apply, so import
/// rejects it rather than silently dropping them. Bump when the export shape
/// changes incompatibly.
const SESSION_SCHEMA_VERSION: u64 = 1;

/// The writable attributes of `cookie set`, grouped so the call site names each
/// one — two adjacent bools (`http_only`/`secure`) are easy to transpose
/// positionally — and the handler signature stays a single argument, mirroring
/// how `do_capture` takes a `CaptureOpts` rather than a long flag list.
pub(super) struct CookieSetSpec<'a> {
    pub url: &'a str,
    pub name: &'a str,
    pub value: &'a str,
    pub http_only: bool,
    pub secure: bool,
    pub same_site: Option<SameSite>,
    pub expires: Option<f64>,
}

impl LocalTransport {
    // ── Cookies ──────────────────────────────────────────────────────────

    pub(super) async fn do_cookie_list(&self, url: &str) -> Result<ResponseData> {
        let cookies = self
            .page
            .send("Network.getCookies", Some(json!({"urls": [url]})))
            .await?;
        let arr = cookies
            .get("cookies")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let parsed: Vec<CookieInfo> = arr.into_iter().map(parse_cdp_cookie).collect();
        Ok(ResponseData::Cookies { cookies: parsed })
    }

    pub(super) async fn do_cookie_set(&self, spec: CookieSetSpec<'_>) -> Result<ResponseData> {
        let mut params = json!({
            "url": spec.url,
            "name": spec.name,
            "value": spec.value,
            "httpOnly": spec.http_only,
            "secure": spec.secure,
        });
        // `Unspecified` (and an omitted flag) means "no SameSite attribute" —
        // leave it off so Chrome applies its own default rather than pinning one.
        if let Some(ss) = spec.same_site {
            let wire = same_site_to_cdp(&ss);
            if !wire.is_empty() {
                params["sameSite"] = wire.into();
            }
        }
        // Absolute Unix-epoch expiry; omitted = a session cookie (CDP's default).
        if let Some(exp) = spec.expires {
            params["expires"] = exp.into();
        }
        let result = self.page.send("Network.setCookie", Some(params)).await?;
        // `Network.setCookie` reports `success:false` when Chrome refuses the
        // cookie — SameSite=None without Secure, a `__Host-`/`__Secure-` name
        // whose attributes break the prefix rules, an invalid domain/value.
        // Reporting success then would tell the agent an auth cookie is set when
        // it silently is not.
        if result.get("success").and_then(|v| v.as_bool()) == Some(false) {
            return Ok(ResponseData::CookieResult {
                success: false,
                deleted: None,
                error: Some(WebPilotError::InvalidArgument {
                    detail: format!(
                        "Chrome refused to set cookie '{}' — common causes: SameSite=None \
                         without --secure, a __Host-/__Secure- name that doesn't meet the \
                         prefix rules, or an invalid domain/value",
                        spec.name
                    ),
                }),
            });
        }
        Ok(ResponseData::CookieResult {
            success: true,
            deleted: None,
            error: None,
        })
    }

    pub(super) async fn do_cookie_delete(&self, url: &str, name: &str) -> Result<ResponseData> {
        // List first: a delete of a cookie that does not exist must be the
        // typed CookieNotFound (`cookie get`'s contract), never a silent
        // success — and same-name cookies coexist across scopes (a `.domain`
        // legacy cookie beside a host-only one, different paths), so EVERY
        // matching scope is deleted precisely (name+domain+path) and the count
        // reported, rather than whatever a bare url+name delete happens to hit.
        let listed = self
            .page
            .send("Network.getCookies", Some(json!({"urls": [url]})))
            .await?;
        let matches: Vec<Value> = listed
            .get("cookies")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|c| c.get("name").and_then(|v| v.as_str()) == Some(name))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if matches.is_empty() {
            return Err(WebPilotError::CookieNotFound {
                name: name.to_string(),
            }
            .into());
        }
        for c in &matches {
            let mut params = json!({
                "name": name,
                "domain": c.get("domain"),
                "path": c.get("path"),
            });
            // A PARTITIONED cookie is deleted only within its partition —
            // measured: `Network.deleteCookies` without the key leaves the
            // partitioned cookie alive, while the match above came from the
            // partition-SPANNING `getCookies`, so "Deleted 1" would be a lie
            // the survival check catches. Thread the matched cookie's own key.
            if let Some(pk) = c.get("partitionKey") {
                params["partitionKey"] = pk.clone();
            }
            self.page
                .send("Network.deleteCookies", Some(params))
                .await?;
        }
        Ok(ResponseData::CookieResult {
            success: true,
            deleted: Some(matches.len() as u64),
            error: None,
        })
    }

    // ── Console / network monitoring ─────────────────────────────────────

    pub(super) async fn do_console_start(&self) -> Result<ResponseData> {
        self.page.evaluate(&with_cap(CONSOLE_INSTALL_JS)).await?;
        self.console_monitoring
            .store(true, std::sync::atomic::Ordering::Release);
        super::persist_monitor_armed(super::Monitor::Console, self.persisted_context_key())?;
        Ok(ok_command_result())
    }

    pub(super) async fn do_console_read(&self, since: Option<u64>) -> Result<ResponseData> {
        // Reading before `console start` would return an empty buffer (exit 0) —
        // indistinguishable from "the page logged nothing" — so an agent could
        // conclude there were no console messages when the monitor was simply
        // never armed. Fail loud instead, so the absence of a hook is never read
        // as the absence of output.
        if !self
            .console_monitoring
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(WebPilotError::InvalidArgument {
                detail: "console monitoring is not active — run `webpilot console start` first"
                    .into(),
            }
            .into());
        }
        // Read the entries AND whether any were EVICTED in one round-trip —
        // `truncated` is driven by the eviction flag the hook sets on a real
        // `shift()`, not by `length >= cap`: a buffer sitting at exactly the cap
        // with nothing dropped yet must not read as truncated (a false "some
        // entries are missing"). `undefined` (no hook in THIS document) is
        // distinct from an empty buffer: the armed flag survives a navigation
        // whose re-arm was suppressed by an `eval` policy deny, and an empty
        // success there would read as "the page logged nothing" while the
        // monitor was in fact off.
        let result = self
            .page
            .evaluate(&format!(
                "(()=>{{const a=window.__webpilot_console;if(a===undefined)return null;\
                  return{{entries:a.filter(e=>e.timestamp>={}),truncated:window.__webpilot_console_dropped===true}};}})()",
                since.unwrap_or(0),
            ))
            .await?;
        if result.is_null() {
            return Err(WebPilotError::InvalidArgument {
                detail: MONITOR_NOT_INSTALLED_CONSOLE.into(),
            }
            .into());
        }
        let truncated = result
            .get("truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let entries: Vec<ConsoleEntry> = result
            .get("entries")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        let level = v
                            .get("level")
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse::<ConsoleLevel>().ok())?;
                        let message = v
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let timestamp = v.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0);
                        Some(ConsoleEntry {
                            level,
                            message,
                            timestamp,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(ResponseData::ConsoleEntries { entries, truncated })
    }

    pub(super) async fn do_console_clear(&self) -> Result<ResponseData> {
        // Sentinel-preserving: an unconditional `= []` would CREATE the buffer
        // in a document whose hook was never installed (an `eval` deny
        // suppressed the re-arm), and the read's hook-absent guard — which
        // keys on `undefined` — would then report an empty success while the
        // monitor is in fact off. Clear only what exists; absent is the same
        // typed signal the read gives.
        let result = self
            .page
            .evaluate(
                "(()=>{const a=window.__webpilot_console;if(a===undefined)return null;window.__webpilot_console=[];window.__webpilot_console_dropped=false;return true;})()",
            )
            .await?;
        if result.is_null() {
            return Err(WebPilotError::InvalidArgument {
                detail: MONITOR_NOT_INSTALLED_CONSOLE.into(),
            }
            .into());
        }
        Ok(ok_command_result())
    }

    pub(super) async fn do_network_start(&self) -> Result<ResponseData> {
        self.page.evaluate(&with_cap(NETWORK_INSTALL_JS)).await?;
        self.network_monitoring
            .store(true, std::sync::atomic::Ordering::Release);
        super::persist_monitor_armed(super::Monitor::Network, self.persisted_context_key())?;
        Ok(ok_command_result())
    }

    /// Re-arm any active monitors after a navigation wiped their `window`
    /// hooks. A no-op until `console start` / `network start` has run, so a
    /// plain navigation pays nothing. Re-arming re-checks policy rather than
    /// extending the grant made at `start` time: an `eval` deny issued after
    /// a monitor started stops its JS from being injected ever again, instead
    /// of riding along inside permitted navigation commands.
    pub(super) async fn reinstall_monitors(&self) {
        use std::sync::atomic::Ordering::Acquire;
        use webpilot::protocol::Command;
        if self.console_monitoring.load(Acquire)
            && crate::policy::enforce(&Command::ConsoleStart).is_ok()
        {
            let _ = self.page.evaluate(&with_cap(CONSOLE_INSTALL_JS)).await;
        }
        if self.network_monitoring.load(Acquire)
            && crate::policy::enforce(&Command::NetworkStart).is_ok()
        {
            let _ = self.page.evaluate(&with_cap(NETWORK_INSTALL_JS)).await;
        }
    }

    pub(super) async fn do_network_read(&self, since: Option<u64>) -> Result<ResponseData> {
        // See `do_console_read`: an empty read before `network start` would read
        // as "no requests" rather than "monitor not armed". Fail loud.
        if !self
            .network_monitoring
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(WebPilotError::InvalidArgument {
                detail: "network monitoring is not active — run `webpilot network start` first"
                    .into(),
            }
            .into());
        }
        // `undefined` (no hook in THIS document) is distinct from an empty
        // buffer — see `do_console_read`. `truncated` is the eviction flag, not
        // `length >= cap` (also see `do_console_read`).
        let js = format!(
            "(()=>{{const a=window.__webpilot_network;if(a===undefined)return null;\
              return{{entries:a.filter(e=>e.timestamp>={}),truncated:window.__webpilot_network_dropped===true}};}})()",
            since.unwrap_or(0),
        );
        let result = self.page.evaluate(&js).await?;
        if result.is_null() {
            return Err(WebPilotError::InvalidArgument {
                detail: MONITOR_NOT_INSTALLED_NETWORK.into(),
            }
            .into());
        }
        let truncated = result
            .get("truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let arr = result
            .get("entries")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let requests: Vec<NetworkEntry> = arr
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();
        Ok(ResponseData::NetworkEntries {
            entries: requests,
            truncated,
        })
    }

    pub(super) async fn do_network_clear(&self) -> Result<ResponseData> {
        // Sentinel-preserving — see do_console_clear.
        let result = self
            .page
            .evaluate(
                "(()=>{const a=window.__webpilot_network;if(a===undefined)return null;window.__webpilot_network=[];window.__webpilot_network_dropped=false;return true;})()",
            )
            .await?;
        if result.is_null() {
            return Err(WebPilotError::InvalidArgument {
                detail: MONITOR_NOT_INSTALLED_NETWORK.into(),
            }
            .into());
        }
        Ok(ok_command_result())
    }

    // ── Session ──────────────────────────────────────────────────────────

    pub(super) async fn do_session_export(&self) -> Result<ResponseData> {
        // Browser-wide cookies (scoped to this context when in `--context`),
        // not just the current URL — matches what `chrome.cookies.getAll({})`
        // returns in browser mode. Both modes serialize as `Vec<CookieInfo>`
        // so a session exported in either mode imports cleanly into the other.
        let raw = self
            .browser
            .get_all_cookies(self.browser_context_id.as_deref())
            .await?;
        let cookies: Vec<CookieInfo> = raw.into_iter().map(parse_cdp_cookie).collect();

        // A failed storage read must fail the export: silently writing a
        // session file with empty storage would import back as data loss.
        let storage = self
            .invoke_bridge(&json!({"type": "exportStorage"}))
            .await?;
        let storage = Self::parse_bridge_response(storage)?;

        let data = json!({
            "version": SESSION_SCHEMA_VERSION,
            "exported_at": epoch_ms() as u64,
            // Storage is origin-scoped; the bridge records whose it is so the
            // import can refuse to write it into a different origin.
            "origin": storage.get("origin"),
            "cookies": cookies,
            "local_storage": storage.get("localStorage"),
            "session_storage": storage.get("sessionStorage"),
        });

        let path = webpilot::dirs::artifact_path("session", "json");
        // Atomic temp+rename so a crash mid-write can't leave a torn session file
        // that a later `session import` would read as malformed.
        webpilot::dirs::atomic_write(&path, serde_json::to_string_pretty(&data)?.as_bytes())?;

        Ok(ResponseData::SessionExport {
            path: path.to_string_lossy().into_owned(),
        })
    }

    pub(super) async fn do_session_import(&self, data: &str) -> Result<ResponseData> {
        let parsed: Value =
            serde_json::from_str(data).map_err(|e| WebPilotError::InvalidArgument {
                detail: format!("session JSON parse error: {e}"),
            })?;

        // An exported session is a JSON object. An array/string/number/null
        // parses fine but reaches every `parsed.get(...)` as `None`, so without
        // this guard it would fall straight through to `success: true` — telling
        // the agent the session imported while nothing was applied. Reject the
        // shape loudly, identically in both modes.
        if !parsed.is_object() {
            return Err(WebPilotError::InvalidArgument {
                detail: "session must be a JSON object".into(),
            }
            .into());
        }

        // Honor the `version` the export stamps: a file from a NEWER schema may
        // carry fields this binary doesn't understand, so importing it would
        // silently drop them and report success — handing the agent a session
        // quietly missing state. Reject any numeric version above what we support
        // rather than half-apply it. Read it as `f64`, not `as_u64`: a non-integer
        // like `1.5` is `None` to `as_u64` and would slip through as "absent",
        // while the browser's plain numeric `>` comparison rejects it — so the two
        // modes would disagree. (A missing or non-numeric version is a
        // hand-written/legacy file; accept it as the current schema.)
        if let Some(v) = parsed.get("version").and_then(Value::as_f64)
            && v > SESSION_SCHEMA_VERSION as f64
        {
            return Err(WebPilotError::InvalidArgument {
                detail: format!(
                    "session was exported by a newer WebPilot (schema v{v}); this binary supports up to v{SESSION_SCHEMA_VERSION} — upgrade to import it"
                ),
            }
            .into());
        }

        // `cookies`, when present, must be an array — a non-array would be
        // silently dropped here and iterated character by character in browser
        // mode. Reject the malformed shape loudly, identically in both modes.
        if let Some(c) = parsed.get("cookies")
            && !c.is_array()
        {
            return Err(WebPilotError::InvalidArgument {
                detail: "session `cookies` must be an array".into(),
            }
            .into());
        }

        // A present storage map must be a plain object (or null) OF STRING
        // VALUES. Validate both shape and value types BEFORE the cookie loop so a
        // malformed file rejects without first applying its cookies — the same
        // fail-up-front order browser mode keeps, so a failed import leaves
        // identical state in both. The bridge re-checks at its sink; this keeps
        // the import atomic (no half-applied cookies behind a storage reject).
        for key in ["local_storage", "session_storage"] {
            let Some(v) = parsed.get(key) else { continue };
            if v.is_null() {
                continue;
            }
            let Some(map) = v.as_object() else {
                return Err(WebPilotError::InvalidArgument {
                    detail: format!("session `{key}` must be an object"),
                }
                .into());
            };
            // Web Storage holds only strings; a non-string value would coerce to
            // garbage ("[object Object]"), so reject it rather than import a lie.
            if map.values().any(|val| !val.is_string()) {
                return Err(WebPilotError::InvalidArgument {
                    detail: format!("session `{key}` values must be strings"),
                }
                .into());
            }
        }

        // Storage to actually apply — an empty/absent map is a no-op (matching the
        // browser `hasStorage` gate), so it needs neither the bridge nor the frame.
        let local_storage = parsed.get("local_storage");
        let session_storage = parsed.get("session_storage");
        let has_storage = storage_to_import(&parsed);
        // Apply storage BEFORE the cookies. Storage is the quota-prone, bulky
        // part, and it imports through the active frame's bridge — so a write
        // the page rejects (a vanished frame → FrameNotFound, or a localStorage
        // quota overflow) must fail up front, before any cookie is committed.
        // Otherwise a half-import leaves the agent an authenticated session
        // (cookies set) sitting on inconsistent app state (storage that could
        // not land) — subtly wrong page behaviour the agent can't see. With
        // storage first, the same failure leaves no cookies, so the page is
        // merely logged-out, not authenticated-but-inconsistent. A successful
        // import is unaffected: both halves land regardless of order.
        if has_storage {
            self.bridge_context_id().await?;
            // A storage write the page rejected (quota) is surfaced by the
            // bridge as a typed error — parse it rather than treating any
            // non-throwing reply as success.
            let resp = self
                .invoke_bridge(&json!({
                    "type": "importStorage",
                    // The bridge enforces the export's origin against the page
                    // it is about to write — origin-scoped state must not land
                    // on a different origin under a success status.
                    "origin": parsed.get("origin").cloned().unwrap_or(Value::Null),
                    "localStorage": local_storage.cloned().unwrap_or_else(|| json!({})),
                    "sessionStorage": session_storage.cloned().unwrap_or_else(|| json!({})),
                }))
                .await?;
            Self::parse_bridge_response(resp)?;
        }

        let mut cookies_failed = 0usize;
        let mut cookies_malformed = 0usize;
        let mut cookies_total = 0usize;
        if let Some(arr) = parsed.get("cookies").and_then(|v| v.as_array()) {
            cookies_total = arr.len();
            for v in arr {
                // A row that parses as neither a `CookieInfo` (CLI shape) nor a
                // raw CDP cookie is counted, not silently dropped — losing a
                // cookie while reporting success would hand the agent a session
                // that is quietly missing some of what the file held.
                let Ok(info) = serde_json::from_value::<CookieInfo>(v.clone()) else {
                    cookies_malformed += 1;
                    continue;
                };
                // Count a refusal too, not just a transport error: a cookie
                // Chrome rejects (SameSite=None without Secure, a broken
                // `__Host-`/`__Secure-` prefix, an invalid domain) comes back as
                // `{success:false}`, and ignoring it would restore a session
                // silently missing auth cookies while reporting full success.
                let set_failed = match self
                    .page
                    .send("Network.setCookie", Some(cookie_info_to_cdp(&info)))
                    .await
                {
                    Ok(r) => r.get("success").and_then(|v| v.as_bool()) == Some(false),
                    Err(_) => true,
                };
                if set_failed {
                    cookies_failed += 1;
                }
            }
        }
        // A cookie the browser refused, or a malformed row that couldn't be
        // parsed, is a partial failure the agent must see — never a success that
        // silently imported less than the file contained.
        if cookies_failed > 0 || cookies_malformed > 0 {
            let mut reasons = Vec::new();
            if cookies_failed > 0 {
                reasons.push(format!("{cookies_failed} refused by the browser"));
            }
            if cookies_malformed > 0 {
                reasons.push(format!("{cookies_malformed} malformed"));
            }
            return Ok(ResponseData::SessionResult {
                success: false,
                error: Some(WebPilotError::Other {
                    detail: format!(
                        "{} of {cookies_total} cookies not imported ({})",
                        cookies_failed + cookies_malformed,
                        reasons.join(", "),
                    ),
                }),
            });
        }

        Ok(ResponseData::SessionResult {
            success: true,
            error: None,
        })
    }
}

/// Whether a parsed session-import payload carries storage to write — the half
/// that runs through the ACTIVE page's bridge. Cookie-only payloads are
/// browser-global (any target's session lands them in the shared jar) and need
/// no page. One predicate drives both the vanished-pin classification in `send`
/// and the import itself, so they can never disagree — and it mirrors the
/// browser worker's `hasStorage` gate exactly.
pub(super) fn storage_to_import(parsed: &Value) -> bool {
    ["local_storage", "session_storage"]
        .iter()
        .filter_map(|k| parsed.get(k))
        .any(|v| v.as_object().is_some_and(|m| !m.is_empty()))
}

fn ok_command_result() -> ResponseData {
    ResponseData::CommandResult {
        success: true,
        value: None,
        error: None,
    }
}

/// CDP `Network.setCookie` spells SameSite as `Strict`/`Lax`/`None`, with an
/// empty string meaning "omit the attribute" (`Unspecified`). One source for
/// both the manual `cookie set` and a session import's per-cookie write.
fn same_site_to_cdp(s: &SameSite) -> &'static str {
    match s {
        SameSite::Strict => "Strict",
        SameSite::Lax => "Lax",
        SameSite::None => "None",
        SameSite::Unspecified => "",
    }
}

fn cookie_info_to_cdp(c: &CookieInfo) -> Value {
    let same_site = same_site_to_cdp(&c.same_site);
    let mut params = json!({
        "name": c.name,
        "value": c.value,
        "path": c.path,
        "secure": c.secure,
        "httpOnly": c.http_only,
    });
    if c.host_only {
        // Host-only: set by URL with no `domain`, so Chrome scopes the cookie to
        // exactly its host and a round-trip can't widen it to subdomains.
        let scheme = if c.secure { "https" } else { "http" };
        let host = c.domain.trim_start_matches('.');
        params["url"] = format!("{scheme}://{host}{}", c.path).into();
    } else {
        params["domain"] = c.domain.clone().into();
    }
    if !same_site.is_empty() {
        params["sameSite"] = same_site.into();
    }
    if let Some(expires) = c.expiration {
        params["expires"] = expires.into();
    }
    if let Some(pk) = &c.partition_key {
        // CHIPS: restore the cookie into its original partition — omitting the
        // key would create an unpartitioned twin instead of the cookie the
        // partitioned (embedded) context actually sends.
        params["partitionKey"] = json!({
            "topLevelSite": pk.top_level_site,
            "hasCrossSiteAncestor": pk.has_cross_site_ancestor,
        });
    }
    params
}

fn parse_cdp_cookie(c: Value) -> CookieInfo {
    let same_site = match c.get("sameSite").and_then(|v| v.as_str()) {
        Some("Strict") | Some("strict") => SameSite::Strict,
        Some("Lax") | Some("lax") => SameSite::Lax,
        Some("None") | Some("none") | Some("no_restriction") => SameSite::None,
        _ => SameSite::Unspecified,
    };
    let domain = c
        .get("domain")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    CookieInfo {
        name: c
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        value: c
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        // A leading dot is CDP's marker for a domain-scoped cookie; its absence
        // means host-only (the exact host, no subdomains) — the RFC 6265 shape
        // CDP and chrome.cookies agree on.
        host_only: !domain.starts_with('.'),
        domain,
        path: c
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        secure: c.get("secure").and_then(|v| v.as_bool()).unwrap_or(false),
        http_only: c.get("httpOnly").and_then(|v| v.as_bool()).unwrap_or(false),
        same_site,
        // CDP marks a session cookie (no expiry) with the sentinel `expires:
        // -1`. Storing that verbatim is a data-loss trap: on `session import`
        // `cookie_info_to_cdp` forwards a present `expiration` as `expires`, and
        // `Network.setCookie` reads any `expires` as an ABSOLUTE time — -1 is
        // epoch-minus-one-second, already expired — so Chrome drops the cookie.
        // Map the sentinel (any negative) to `None`, so a session cookie
        // round-trips as one (import then omits `expires`) and `cookie list`
        // shows it as a session cookie, not a bogus 1969 expiry.
        expiration: match c.get("expires").and_then(|v| v.as_f64()) {
            Some(secs) if secs >= 0.0 => Some(secs),
            _ => None,
        },
        // CHIPS: carry the partition key — it is part of the cookie's IDENTITY
        // (see CookieInfo::partition_key), so a round-trip must preserve it.
        partition_key: c.get("partitionKey").and_then(|pk| {
            Some(PartitionKey {
                top_level_site: pk.get("topLevelSite")?.as_str()?.to_string(),
                has_cross_site_ancestor: pk
                    .get("hasCrossSiteAncestor")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        }),
    }
}

/// Max entries kept in each MAIN-world monitor ring buffer. The install scripts
/// below evict the oldest past this (their `$CAP` placeholder), and a read reports
/// `truncated` at this cap — one source, so eviction and the flag cannot drift.
const MONITOR_BUFFER_CAP: usize = 500;

/// The armed flag is set but THIS document carries no hook: the re-arm after a
/// navigation was suppressed (an `eval` policy deny stops monitor injection) or
/// the document loaded outside WebPilot's drive. An empty success here would
/// read as "the page was quiet" while the monitor was in fact off.
const MONITOR_NOT_INSTALLED_CONSOLE: &str = "the console monitor is not installed in this document — an `eval` policy deny suppresses re-arming after navigation; check `webpilot policy list`, then run `webpilot console start`";
const MONITOR_NOT_INSTALLED_NETWORK: &str = "the network monitor is not installed in this document — an `eval` policy deny suppresses re-arming after navigation; check `webpilot policy list`, then run `webpilot network start`";

/// Substitute the ring-buffer cap into a monitor install script. The page-side
/// eviction (`> $CAP`) and the Rust-side `truncated` flag both derive from
/// `MONITOR_BUFFER_CAP` through this, so changing the cap can never leave them
/// disagreeing (which would tell an agent the buffer is complete after silent drops).
fn with_cap(install_js: &str) -> String {
    install_js.replace("$CAP", &MONITOR_BUFFER_CAP.to_string())
}

const CONSOLE_INSTALL_JS: &str = r#"
// Always (re-)attach the recorder. Gating on `window.__webpilot_console`
// alone fails after `console clear` because an empty array is truthy and
// the patch wouldn't reinstall. A separate sentinel keeps `start` idempotent
// without that hazard.
if (!Array.isArray(window.__webpilot_console)) {
    window.__webpilot_console = [];
}
if (!window.__webpilot_console_patched) {
    window.__webpilot_console_patched = true;
    // Capture Date.now at install so a page that later booby-traps it (a
    // throwing getter) can't break the recording — and even if recording does
    // throw, the page's OWN console call still fires (see the try below). The
    // monitor's honest boundary is "may miss an entry", never "breaks the page".
    const nowFn = Date.now;
    // Clip a captured message like the DOM capture clips text: a runaway
    // `console.log("x".repeat(5e7))` must not balloon the buffer or the read's
    // CDP payload. CODEPOINT-safe via Array.from (like bridge.js's clip): a
    // bare `slice` cuts by UTF-16 code unit and can split an astral pair into a
    // lone surrogate, which breaks the entry's JSON serialization through CDP
    // returnByValue / native messaging. The marker keeps the clip visible.
    const MAX = 4096;
    const clip = (s) => { if (s.length <= MAX) return s; const cps = Array.from(s); return cps.length > MAX ? cps.slice(0, MAX).join("") + "…[" + cps.length + " chars]" : s; };
    const orig = { log: console.log, error: console.error, warn: console.warn, info: console.info, debug: console.debug };
    ["log", "error", "warn", "info", "debug"].forEach(m => {
        console[m] = (...args) => {
            try {
                const msg = clip(args.map(a => { try { return String(a); } catch { return "[object]"; } }).join(" "));
                const buf = window.__webpilot_console;
                buf.push({ level: m, message: msg, timestamp: nowFn() });
                // Evict the oldest past the cap and RECORD that an eviction
                // happened: the read's `truncated` flag is driven by this, not
                // by `length >= cap`, so a buffer sitting at exactly the cap
                // (nothing dropped yet) isn't falsely reported truncated.
                if (buf.length > $CAP) { buf.shift(); window.__webpilot_console_dropped = true; }
            } catch (e) {}
            orig[m].apply(console, args);
        };
    });
}
true
"#;

const NETWORK_INSTALL_JS: &str = r#"
if (!window.__webpilot_network_active) {
    window.__webpilot_network_active = true;
    window.__webpilot_network = [];
    // Intrinsics captured at install: a page that later booby-traps Date.now /
    // performance.now (a throwing getter) can't break the recording, and the
    // recording is wrapped so it can never break the page's OWN fetch/XHR
    // (the monitor's honest boundary is "may miss an entry", never "breaks the
    // page"). A captured URL is clipped like the DOM capture so a giant data:
    // URL can't balloon the buffer or the read's CDP payload.
    // Intrinsics captured by binding the receiver, so a page that swaps
    // Date.now / performance.now after install can't skew the recording.
    const nowFn = Date.now;
    const perfObj = performance;
    const perfNowRaw = perfObj.now;
    const perfNow = () => { try { return perfNowRaw.call(perfObj); } catch (e) { return 0; } };
    const MAX = 4096;
    // CODEPOINT-safe clip (a lone surrogate from a split astral pair breaks the
    // entry's JSON serialization — see the console hook).
    const clip = (s) => { if (s.length <= MAX) return s; const cps = Array.from(s); return cps.length > MAX ? cps.slice(0, MAX).join("") + "…[" + cps.length + " chars]" : s; };
    const origFetch = window.fetch;
    window.fetch = function(...args) {
        let entry = null, t0 = 0;
        try {
            const [resource, config] = args;
            // A Request object carries its own url/method (a config override still
            // wins); String(resource) on one logs "[object Request]" and drops the
            // method.
            const isReq = typeof Request !== "undefined" && resource instanceof Request;
            const url = isReq ? resource.url : String(resource);
            const method = config?.method || (isReq ? resource.method : "GET");
            t0 = perfNow();
            // Record in-flight immediately (no status, duration 0) so a read during
            // a slow request sees it; fill in on completion by mutating this entry.
            entry = { type: "fetch", url: clip(url), method, duration_ms: 0, timestamp: nowFn() };
            const buf = window.__webpilot_network;
            buf.push(entry);
            if (buf.length > $CAP) { buf.shift(); window.__webpilot_network_dropped = true; }
        } catch (e) { entry = null; }
        // origFetch can throw SYNCHRONOUSLY (a bad argument — `fetch()` with no
        // args is a TypeError, not a rejected promise). Stamp the recorded entry
        // as errored instead of leaving it in-flight forever, then rethrow so the
        // page sees the same exception.
        let p;
        try {
            p = origFetch.apply(this, args);
        } catch (e) {
            if (entry) { try { entry.error = String(e && e.message || e); entry.duration_ms = Math.round(perfNow() - t0); entry.timestamp = nowFn(); } catch (e2) {} }
            throw e;
        }
        if (!entry) return p;
        return p.then(response => {
            // Re-stamp at completion so `--since` polling, which filters on
            // timestamp, sees the resolved entry; the in-flight start time would
            // sit before a cursor taken after the request began.
            try { entry.status = response.status; entry.duration_ms = Math.round(perfNow() - t0); entry.timestamp = nowFn(); } catch (e) {}
            return response;
        }).catch(err => {
            try { entry.error = err.message; entry.duration_ms = Math.round(perfNow() - t0); entry.timestamp = nowFn(); } catch (e) {}
            throw err;
        });
    };
    const xhrProto = XMLHttpRequest.prototype;
    const origOpen = xhrProto.open;
    const origSend = xhrProto.send;
    const xhrMeta = new WeakMap();
    xhrProto.open = function(m, u, ...a) {
        try { xhrMeta.set(this, { method: m, url: String(u) }); } catch (e) {}
        return origOpen.apply(this, [m, u, ...a]);
    };
    xhrProto.send = function(...a) {
        let entry = null, t0 = 0;
        try {
            t0 = perfNow();
            const meta = xhrMeta.get(this) || {};
            entry = { type: "xhr", url: clip(meta.url || ""), method: meta.method || "GET", duration_ms: 0, timestamp: nowFn() };
            const buf = window.__webpilot_network;
            buf.push(entry);
            if (buf.length > $CAP) { buf.shift(); window.__webpilot_network_dropped = true; }
            // status===0 covers abort, timeout AND network/CORS failure alike, so
            // read the actual terminal event instead of labelling every one a
            // "Network error" — an aborted request the page itself cancelled is not
            // a network failure.
            let terminalError;
            this.addEventListener("abort", () => { terminalError = "aborted"; }, { once: true });
            this.addEventListener("timeout", () => { terminalError = "timeout"; }, { once: true });
            this.addEventListener("error", () => { terminalError = "Network error"; }, { once: true });
            this.addEventListener("loadend", () => {
                try { entry.status = this.status || undefined; entry.error = terminalError; entry.duration_ms = Math.round(perfNow() - t0); entry.timestamp = nowFn(); } catch (e) {}
            }, { once: true });
        } catch (e) { entry = null; }
        return origSend.apply(this, a);
    };
}
true
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> CookieInfo {
        CookieInfo {
            name: "session".into(),
            value: "abc".into(),
            domain: ".example.com".into(),
            path: "/".into(),
            secure: true,
            http_only: true,
            same_site: SameSite::Lax,
            expiration: None,
            host_only: false,
            partition_key: None,
        }
    }

    #[test]
    fn cookie_info_to_cdp_maps_canonical_fields() {
        let v = cookie_info_to_cdp(&base());
        assert_eq!(v["name"], "session");
        assert_eq!(v["value"], "abc");
        assert_eq!(v["domain"], ".example.com");
        assert_eq!(v["path"], "/");
        assert_eq!(v["secure"], true);
        assert_eq!(v["httpOnly"], true);
        assert_eq!(v["sameSite"], "Lax");
        assert!(v.get("expires").is_none(), "session cookie omits expires");
    }

    #[test]
    fn cookie_info_to_cdp_omits_unspecified_same_site() {
        let mut c = base();
        c.same_site = SameSite::Unspecified;
        let v = cookie_info_to_cdp(&c);
        assert!(v.get("sameSite").is_none());
    }

    #[test]
    fn cookie_info_to_cdp_includes_expires_when_set() {
        let mut c = base();
        c.expiration = Some(1_700_000_000.5);
        let v = cookie_info_to_cdp(&c);
        assert_eq!(v["expires"], 1_700_000_000.5);
    }

    #[test]
    fn cookie_info_to_cdp_each_same_site() {
        for (variant, wire) in [
            (SameSite::Strict, "Strict"),
            (SameSite::Lax, "Lax"),
            (SameSite::None, "None"),
        ] {
            let mut c = base();
            c.same_site = variant.clone();
            let v = cookie_info_to_cdp(&c);
            assert_eq!(v["sameSite"], wire, "variant {variant:?}");
        }
    }

    #[test]
    fn parse_cdp_cookie_round_trip() {
        // A cookie exported via cookie_info_to_cdp and re-parsed via
        // parse_cdp_cookie must preserve every flag.
        // A DOMAIN cookie carries its scope in the `domain` param, so the
        // synthetic param→param round-trip preserves it (a host-only cookie
        // travels by `url` and recovers its domain only through real Chrome).
        let original = CookieInfo {
            name: "x".into(),
            value: "y".into(),
            domain: ".a.b".into(),
            path: "/p".into(),
            secure: false,
            http_only: true,
            same_site: SameSite::Strict,
            expiration: Some(1_700_000_000.0),
            host_only: false,
            partition_key: None,
        };
        let mut raw = cookie_info_to_cdp(&original);
        raw["expires"] = original.expiration.unwrap().into();
        let recovered = parse_cdp_cookie(raw);

        assert_eq!(recovered.name, original.name);
        assert_eq!(recovered.value, original.value);
        assert_eq!(recovered.domain, original.domain);
        assert_eq!(recovered.host_only, original.host_only);
        assert_eq!(recovered.path, original.path);
        assert_eq!(recovered.secure, original.secure);
        assert_eq!(recovered.http_only, original.http_only);
        assert_eq!(recovered.same_site, original.same_site);
        assert_eq!(recovered.expiration, original.expiration);
    }

    #[test]
    fn partitioned_cookie_round_trips_with_its_key() {
        // CHIPS: the partition key is part of the cookie's IDENTITY — a
        // round-trip that dropped it would re-import an unpartitioned twin the
        // partitioned context never sends, under a clean success.
        let mut original = base();
        original.partition_key = Some(PartitionKey {
            top_level_site: "https://example.com".into(),
            has_cross_site_ancestor: true,
        });
        let raw = cookie_info_to_cdp(&original);
        assert_eq!(raw["partitionKey"]["topLevelSite"], "https://example.com");
        assert_eq!(raw["partitionKey"]["hasCrossSiteAncestor"], true);
        let recovered = parse_cdp_cookie(raw);
        assert_eq!(recovered.partition_key, original.partition_key);

        // ...and an unpartitioned cookie stays free of the field entirely.
        let plain = cookie_info_to_cdp(&base());
        assert!(plain.get("partitionKey").is_none());
        assert_eq!(parse_cdp_cookie(plain).partition_key, None);
    }

    #[test]
    fn cookie_info_to_cdp_host_only_uses_url_not_domain() {
        let mut c = base();
        c.host_only = true;
        c.domain = "app.example.com".into();
        c.path = "/".into();
        let v = cookie_info_to_cdp(&c);
        assert!(
            v.get("domain").is_none(),
            "host-only cookie must omit domain"
        );
        assert_eq!(v["url"], "https://app.example.com/");
    }
}
