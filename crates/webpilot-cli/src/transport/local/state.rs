//! State-keeping commands: cookies, console + network monitoring,
//! session export/import, operation policies.

use anyhow::Result;
use serde_json::{Value, json};
use webpilot::WebPilotError;
use webpilot::protocol::ResponseData;
use webpilot::types::{
    ConsoleEntry, ConsoleLevel, CookieInfo, NetworkEntry, PolicyEntry, PolicyVerdict, SameSite,
};

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
        Ok(ok_command_result())
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
        Ok(ResponseData::NetworkLog { requests })
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

        let storage = self
            .invoke_bridge(&json!({"type": "exportStorage"}))
            .await
            .unwrap_or_else(|_| json!({"localStorage": {}, "sessionStorage": {}}));

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

        if let Some(arr) = parsed.get("cookies").and_then(|v| v.as_array()) {
            for v in arr {
                // Tolerate both CookieInfo (CLI shape) and raw CDP rows. Bad
                // entries are skipped — one bad cookie shouldn't fail import.
                let Ok(info) = serde_json::from_value::<CookieInfo>(v.clone()) else {
                    continue;
                };
                let _ = self
                    .page
                    .send("Network.setCookie", Some(cookie_info_to_cdp(&info)))
                    .await;
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

        Ok(ResponseData::SessionResult {
            success: true,
            error: None,
        })
    }

    // ── Operation policies (file-backed store) ───────────────────────────

    pub(super) async fn do_policy_set(
        &self,
        operation: webpilot::types::PolicyKey,
        verdict: PolicyVerdict,
    ) -> Result<ResponseData> {
        let mut store = policy_store::read();
        store.insert(operation, verdict);
        policy_store::write(&store)?;
        Ok(ResponseData::PolicyResult {
            success: true,
            error: None,
        })
    }

    pub(super) async fn do_policy_list(&self) -> Result<ResponseData> {
        // Strict load: surface a corrupt store as an error so `list` agrees with
        // enforcement (which denies on corruption) instead of misreporting "no
        // policies" while everything is in fact denied.
        let store = policy_store::load().map_err(|_| WebPilotError::Other {
            detail: "policy store is invalid; run: webpilot policy clear".into(),
        })?;
        let policies: Vec<PolicyEntry> = store
            .into_iter()
            .map(|(operation, verdict)| PolicyEntry { operation, verdict })
            .collect();
        Ok(ResponseData::Policies { policies })
    }

    pub(super) async fn do_policy_clear(&self) -> Result<ResponseData> {
        policy_store::write(&std::collections::HashMap::new())?;
        Ok(ResponseData::PolicyResult {
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

/// File-backed policy store, shared across CLI invocations.
pub(super) mod policy_store {
    use anyhow::Result;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use webpilot::dirs;
    use webpilot::types::{PolicyKey, PolicyVerdict};

    fn policy_file() -> PathBuf {
        dirs::artifacts_dir().join("policies.json")
    }

    /// Load the store. An absent file is the empty (no-policy) state; any other
    /// read failure or a parse failure is surfaced so enforcement and `list` can
    /// fail closed rather than silently dropping a user's deny rule.
    pub fn load() -> std::io::Result<HashMap<PolicyKey, PolicyVerdict>> {
        match std::fs::read_to_string(policy_file()) {
            Ok(text) => parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(e),
        }
    }

    /// Parse store JSON. All-or-nothing: an unknown operation or verdict makes
    /// the whole store untrusted (returns `Err`) so `denies()` fails closed,
    /// rather than silently dropping the bad entry and letting it through.
    fn parse(text: &str) -> std::io::Result<HashMap<PolicyKey, PolicyVerdict>> {
        let invalid =
            |msg: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_owned());
        let raw: HashMap<String, String> =
            serde_json::from_str(text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut store = HashMap::with_capacity(raw.len());
        for (k, v) in raw {
            let key: PolicyKey = k.parse().map_err(|_| invalid("unknown operation"))?;
            let verdict: PolicyVerdict = v.parse().map_err(|_| invalid("unknown verdict"))?;
            store.insert(key, verdict);
        }
        Ok(store)
    }

    /// Lenient load for management commands: a corrupt store reads as empty so a
    /// `policy set` overwrites it cleanly.
    pub fn read() -> HashMap<PolicyKey, PolicyVerdict> {
        load().unwrap_or_default()
    }

    pub fn write(store: &HashMap<PolicyKey, PolicyVerdict>) -> Result<()> {
        let raw: HashMap<String, String> = store
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        std::fs::write(policy_file(), serde_json::to_string_pretty(&raw)?)?;
        Ok(())
    }

    /// Enforcement predicate shared by every gated operation across both
    /// transports. Fails closed: a store that exists but can't be read or parsed
    /// denies the operation rather than silently allowing it.
    pub fn denies(key: PolicyKey) -> bool {
        match load() {
            Ok(store) => store.get(&key) == Some(&PolicyVerdict::Deny),
            Err(_) => true,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::parse;

        #[test]
        fn valid_store_parses() {
            assert_eq!(parse(r#"{"click":"deny","eval":"allow"}"#).unwrap().len(), 2);
        }

        #[test]
        fn empty_store_is_ok() {
            assert!(parse("{}").unwrap().is_empty());
        }

        // Fail closed: any untrusted content makes the whole store an error so
        // `denies()` denies rather than letting an operation slip through.
        #[test]
        fn malformed_json_is_error() {
            assert!(parse("{not json").is_err());
        }

        #[test]
        fn unknown_operation_is_error() {
            assert!(parse(r#"{"teleport":"deny"}"#).is_err());
        }

        #[test]
        fn unknown_verdict_is_error() {
            assert!(parse(r#"{"click":"maybe"}"#).is_err());
        }
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
    xhrProto.open = function(m, u, ...a) {
        this.__wp_method = m; this.__wp_url = String(u);
        return origOpen.apply(this, [m, u, ...a]);
    };
    xhrProto.send = function(...a) {
        const t0 = performance.now();
        this.addEventListener("loadend", () => {
            window.__webpilot_network.push({
                type: "xhr", url: this.__wp_url || "", method: this.__wp_method || "GET",
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
