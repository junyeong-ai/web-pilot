//! State-keeping commands: cookies, console + network monitoring,
//! session export/import.

use anyhow::Result;
use serde_json::{Value, json};
use webpilot::WebPilotError;
use webpilot::protocol::ResponseData;
use webpilot::types::{ConsoleEntry, ConsoleLevel, CookieInfo, NetworkEntry, SameSite};

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
            error: None,
        })
    }

    pub(super) async fn do_cookie_delete(&self, url: &str, name: &str) -> Result<ResponseData> {
        self.page
            .send(
                "Network.deleteCookies",
                Some(json!({"url": url, "name": name})),
            )
            .await?;
        Ok(ResponseData::CookieResult {
            success: true,
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
        // Read the entries AND whether the buffer is at its cap in one round-trip
        // — `truncated` flags a full buffer (older entries possibly evicted) so a
        // missing early entry never reads as a confident absence.
        let result = self
            .page
            .evaluate(&format!(
                "(()=>{{const a=window.__webpilot_console||[];\
                  return{{entries:a.filter(e=>e.timestamp>={}),truncated:a.length>={}}};}})()",
                since.unwrap_or(0),
                MONITOR_BUFFER_CAP,
            ))
            .await?;
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
        self.page
            .evaluate("window.__webpilot_console = []; true")
            .await?;
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
        let js = format!(
            "(()=>{{const a=window.__webpilot_network||[];\
              return{{entries:a.filter(e=>e.timestamp>={}),truncated:a.length>={}}};}})()",
            since.unwrap_or(0),
            MONITOR_BUFFER_CAP,
        );
        let result = self.page.evaluate(&js).await?;
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
        self.page
            .evaluate("window.__webpilot_network = []; true")
            .await?;
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
        let has_storage = [local_storage, session_storage]
            .into_iter()
            .flatten()
            .any(|v| v.as_object().is_some_and(|m| !m.is_empty()));
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
    }
}

/// Max entries kept in each MAIN-world monitor ring buffer. The install scripts
/// below evict the oldest past this (their `$CAP` placeholder), and a read reports
/// `truncated` at this cap — one source, so eviction and the flag cannot drift.
const MONITOR_BUFFER_CAP: usize = 500;

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
    const orig = { log: console.log, error: console.error, warn: console.warn, info: console.info, debug: console.debug };
    ["log", "error", "warn", "info", "debug"].forEach(m => {
        console[m] = (...args) => {
            window.__webpilot_console.push({
                level: m,
                message: args.map(a => { try { return String(a); } catch { return "[object]"; } }).join(" "),
                timestamp: Date.now(),
            });
            if (window.__webpilot_console.length > $CAP) window.__webpilot_console.shift();
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
    const origFetch = window.fetch;
    window.fetch = function(...args) {
        const [resource, config] = args;
        // A Request object carries its own url/method (a config override still
        // wins); String(resource) on one logs "[object Request]" and drops the
        // method.
        const isReq = typeof Request !== "undefined" && resource instanceof Request;
        const url = isReq ? resource.url : String(resource);
        const method = config?.method || (isReq ? resource.method : "GET");
        const t0 = performance.now();
        // Record in-flight immediately (no status, duration 0) so a read during a
        // slow request sees it; fill in on completion by mutating this entry.
        const entry = { type: "fetch", url, method, duration_ms: 0, timestamp: Date.now() };
        window.__webpilot_network.push(entry);
        if (window.__webpilot_network.length > $CAP) window.__webpilot_network.shift();
        return origFetch.apply(this, args).then(response => {
            entry.status = response.status;
            entry.duration_ms = Math.round(performance.now() - t0);
            // Re-stamp at completion so `--since` polling, which filters on
            // timestamp, sees the resolved entry; the in-flight start time would
            // sit before a cursor taken after the request began.
            entry.timestamp = Date.now();
            return response;
        }).catch(err => {
            entry.error = err.message;
            entry.duration_ms = Math.round(performance.now() - t0);
            entry.timestamp = Date.now();
            throw err;
        });
    };
    const xhrProto = XMLHttpRequest.prototype;
    const origOpen = xhrProto.open;
    const origSend = xhrProto.send;
    const xhrMeta = new WeakMap();
    xhrProto.open = function(m, u, ...a) {
        xhrMeta.set(this, { method: m, url: String(u) });
        return origOpen.apply(this, [m, u, ...a]);
    };
    xhrProto.send = function(...a) {
        const t0 = performance.now();
        const meta = xhrMeta.get(this) || {};
        // Record in-flight at send (no status, duration 0), updated on loadend.
        const entry = { type: "xhr", url: meta.url || "", method: meta.method || "GET", duration_ms: 0, timestamp: Date.now() };
        window.__webpilot_network.push(entry);
        if (window.__webpilot_network.length > $CAP) window.__webpilot_network.shift();
        // status===0 covers abort, timeout AND network/CORS failure alike, so
        // read the actual terminal event instead of labelling every one a
        // "Network error" — an aborted request the page itself cancelled is not a
        // network failure.
        let terminalError;
        this.addEventListener("abort", () => { terminalError = "aborted"; }, { once: true });
        this.addEventListener("timeout", () => { terminalError = "timeout"; }, { once: true });
        this.addEventListener("error", () => { terminalError = "Network error"; }, { once: true });
        this.addEventListener("loadend", () => {
            entry.status = this.status || undefined;
            entry.error = terminalError;
            entry.duration_ms = Math.round(performance.now() - t0);
            entry.timestamp = Date.now();
        }, { once: true });
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
