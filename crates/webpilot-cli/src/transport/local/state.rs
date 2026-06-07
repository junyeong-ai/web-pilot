//! State-keeping commands: cookies, console + network monitoring,
//! session export/import.

use anyhow::Result;
use serde_json::{Value, json};
use webpilot::WebPilotError;
use webpilot::protocol::ResponseData;
use webpilot::types::{ConsoleEntry, ConsoleLevel, CookieInfo, NetworkEntry, SameSite};

use super::{LocalTransport, artifact_path, epoch_ms};

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

    pub(super) async fn do_cookie_set(
        &self,
        url: &str,
        name: &str,
        value: &str,
        http_only: bool,
        secure: bool,
    ) -> Result<ResponseData> {
        self.page
            .send(
                "Network.setCookie",
                Some(json!({
                    "url": url,
                    "name": name,
                    "value": value,
                    "httpOnly": http_only,
                    "secure": secure,
                })),
            )
            .await?;
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
        self.page.evaluate(CONSOLE_INSTALL_JS).await?;
        self.console_monitoring
            .store(true, std::sync::atomic::Ordering::Release);
        super::persist_monitor_armed(super::Monitor::Console, self.persisted_context_key());
        Ok(ok_command_result())
    }

    pub(super) async fn do_console_read(&self) -> Result<ResponseData> {
        let result = self
            .page
            .evaluate("window.__webpilot_console || []")
            .await?;
        let entries: Vec<ConsoleEntry> = result
            .as_array()
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
        Ok(ResponseData::ConsoleEntries { entries })
    }

    pub(super) async fn do_console_clear(&self) -> Result<ResponseData> {
        self.page
            .evaluate("window.__webpilot_console = []; true")
            .await?;
        Ok(ok_command_result())
    }

    pub(super) async fn do_network_start(&self) -> Result<ResponseData> {
        self.page.evaluate(NETWORK_INSTALL_JS).await?;
        self.network_monitoring
            .store(true, std::sync::atomic::Ordering::Release);
        super::persist_monitor_armed(super::Monitor::Network, self.persisted_context_key());
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
            let _ = self.page.evaluate(CONSOLE_INSTALL_JS).await;
        }
        if self.network_monitoring.load(Acquire)
            && crate::policy::enforce(&Command::NetworkStart).is_ok()
        {
            let _ = self.page.evaluate(NETWORK_INSTALL_JS).await;
        }
    }

    pub(super) async fn do_network_read(&self, since: Option<u64>) -> Result<ResponseData> {
        let js = format!(
            "(window.__webpilot_network || []).filter(e => e.timestamp >= {})",
            since.unwrap_or(0)
        );
        let result = self.page.evaluate(&js).await?;
        let arr = result.as_array().cloned().unwrap_or_default();
        let requests: Vec<NetworkEntry> = arr
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();
        Ok(ResponseData::NetworkEntries { entries: requests })
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
        let storage = self.invoke_bridge(&json!({"type": "exportStorage"})).await?;
        let storage = Self::parse_bridge_response(storage)?;

        let data = json!({
            "version": 1,
            "exported_at": epoch_ms() as u64,
            "cookies": cookies,
            "local_storage": storage.get("localStorage"),
            "session_storage": storage.get("sessionStorage"),
        });

        let path = artifact_path("session", "json");
        std::fs::write(&path, serde_json::to_string_pretty(&data)?)?;

        Ok(ResponseData::SessionExport {
            path: path.to_string_lossy().into_owned(),
        })
    }

    pub(super) async fn do_session_import(&self, data: &str) -> Result<ResponseData> {
        let parsed: Value =
            serde_json::from_str(data).map_err(|e| WebPilotError::InvalidArgument {
                detail: format!("session JSON parse error: {e}"),
            })?;

        let mut cookies_failed = 0usize;
        let mut cookies_total = 0usize;
        if let Some(arr) = parsed.get("cookies").and_then(|v| v.as_array()) {
            for v in arr {
                // Tolerate both CookieInfo (CLI shape) and raw CDP rows. Bad
                // entries are skipped — one bad cookie shouldn't fail import.
                let Ok(info) = serde_json::from_value::<CookieInfo>(v.clone()) else {
                    continue;
                };
                cookies_total += 1;
                if self
                    .page
                    .send("Network.setCookie", Some(cookie_info_to_cdp(&info)))
                    .await
                    .is_err()
                {
                    cookies_failed += 1;
                }
            }
        }
        let local_storage = parsed.get("local_storage");
        let session_storage = parsed.get("session_storage");
        if local_storage.is_some() || session_storage.is_some() {
            self.invoke_bridge(&json!({
                "type": "importStorage",
                "localStorage": local_storage.cloned().unwrap_or_else(|| json!({})),
                "sessionStorage": session_storage.cloned().unwrap_or_else(|| json!({})),
            }))
            .await?;
        }

        // A well-formed cookie the browser refused to set is a partial
        // failure the agent must see — never a success that silently
        // imported less than the file contained.
        if cookies_failed > 0 {
            return Ok(ResponseData::SessionResult {
                success: false,
                error: Some(WebPilotError::Other {
                    detail: format!("{cookies_failed} of {cookies_total} cookies failed to set"),
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

fn cookie_info_to_cdp(c: &CookieInfo) -> Value {
    let same_site = match c.same_site {
        SameSite::Strict => "Strict",
        SameSite::Lax => "Lax",
        SameSite::None => "None",
        SameSite::Unspecified => "",
    };
    let mut params = json!({
        "name": c.name,
        "value": c.value,
        "domain": c.domain,
        "path": c.path,
        "secure": c.secure,
        "httpOnly": c.http_only,
    });
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
        domain: c
            .get("domain")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        path: c
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        secure: c.get("secure").and_then(|v| v.as_bool()).unwrap_or(false),
        http_only: c.get("httpOnly").and_then(|v| v.as_bool()).unwrap_or(false),
        same_site,
        expiration: c.get("expires").and_then(|v| v.as_f64()),
    }
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
    const orig = { log: console.log, error: console.error, warn: console.warn, info: console.info };
    ["log", "error", "warn", "info"].forEach(m => {
        console[m] = (...args) => {
            window.__webpilot_console.push({
                level: m,
                message: args.map(a => { try { return String(a); } catch { return "[object]"; } }).join(" "),
                timestamp: Date.now(),
            });
            if (window.__webpilot_console.length > 500) window.__webpilot_console.shift();
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
        const t0 = performance.now();
        return origFetch.apply(this, args).then(response => {
            window.__webpilot_network.push({
                type: "fetch", url: String(resource),
                method: config?.method || "GET", status: response.status,
                duration_ms: Math.round(performance.now() - t0),
                timestamp: Date.now(),
            });
            if (window.__webpilot_network.length > 500) window.__webpilot_network.shift();
            return response;
        }).catch(err => {
            window.__webpilot_network.push({
                type: "fetch", url: String(resource),
                method: config?.method || "GET", error: err.message,
                duration_ms: Math.round(performance.now() - t0),
                timestamp: Date.now(),
            });
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
        this.addEventListener("loadend", () => {
            const meta = xhrMeta.get(this) || {};
            window.__webpilot_network.push({
                type: "xhr", url: meta.url || "", method: meta.method || "GET",
                status: this.status || undefined,
                error: this.status === 0 ? "Network error" : undefined,
                duration_ms: Math.round(performance.now() - t0),
                timestamp: Date.now(),
            });
            if (window.__webpilot_network.length > 500) window.__webpilot_network.shift();
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
        let original = CookieInfo {
            name: "x".into(),
            value: "y".into(),
            domain: "a.b".into(),
            path: "/p".into(),
            secure: false,
            http_only: true,
            same_site: SameSite::Strict,
            expiration: Some(1_700_000_000.0),
        };
        let mut raw = cookie_info_to_cdp(&original);
        raw["expires"] = original.expiration.unwrap().into();
        let recovered = parse_cdp_cookie(raw);

        assert_eq!(recovered.name, original.name);
        assert_eq!(recovered.value, original.value);
        assert_eq!(recovered.domain, original.domain);
        assert_eq!(recovered.path, original.path);
        assert_eq!(recovered.secure, original.secure);
        assert_eq!(recovered.http_only, original.http_only);
        assert_eq!(recovered.same_site, original.same_site);
        assert_eq!(recovered.expiration, original.expiration);
    }
}
