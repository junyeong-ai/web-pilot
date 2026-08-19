//! State-keeping commands: cookies, console + network monitoring,
//! session export/import.

use anyhow::Result;
use serde_json::{Value, json};
use webpilot::WebPilotError;
use webpilot::protocol::ResponseData;
use webpilot::types::{
    ConsoleEntry, ConsoleLevel, ConsoleSource, CookieInfo, NetworkEntry, PartitionKey, SameSite,
};

use crate::cdp::CdpSession;

use super::{LocalTransport, Monitor, epoch_ms};

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
        self.console_monitoring
            .store(true, std::sync::atomic::Ordering::Release);
        super::persist_monitor_armed(Monitor::Console, self.persisted_context_key())?;
        self.ensure_monitor_hook(Monitor::Console).await?;
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
        // distinct from an empty buffer: the armed flag outlives an `eval` policy
        // deny that removed the monitor's injection, and an empty success there
        // would read as "the page logged nothing" while the monitor was in fact
        // off.
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
                        let typed = |field: &str| v.get(field).and_then(|v| v.as_str());
                        let source = typed("source")?.parse::<ConsoleSource>().ok()?;
                        let level = typed("level")?.parse::<ConsoleLevel>().ok()?;
                        let message = typed("message").unwrap_or_default().to_string();
                        let timestamp = v.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0);
                        Some(ConsoleEntry {
                            source,
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
        // in a document whose hook was never installed (an `eval` deny removed
        // the injection), and the read's hook-absent guard — which keys on
        // `undefined` — would then report an empty success while the monitor is
        // in fact off. Clear only what exists; absent is the same typed signal
        // the read gives.
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
        self.network_monitoring
            .store(true, std::sync::atomic::Ordering::Release);
        super::persist_monitor_armed(Monitor::Network, self.persisted_context_key())?;
        self.ensure_monitor_hook(Monitor::Network).await?;
        Ok(ok_command_result())
    }

    /// Reconcile every monitor, best-effort: a command that has nothing to do
    /// with monitoring must not fail because a registration could not be placed,
    /// and the failure is not lost — the record stays unchanged so the next
    /// command retries, and until one succeeds a read reports the hook as absent
    /// rather than returning an empty buffer. `console start` / `network start`
    /// reconcile their own monitor directly, where the failure IS the answer.
    pub(super) async fn ensure_monitor_hooks(&self) {
        for kind in [Monitor::Console, Monitor::Network] {
            let _ = self.ensure_monitor_hook(kind).await;
        }
    }

    /// Bring one monitor's on-new-document registration in line with what it is
    /// currently allowed to do: registered while armed and permitted, removed
    /// otherwise. Registering — rather than injecting once a navigation settles —
    /// is what puts the hook in a document ahead of the document's own scripts,
    /// so a page's startup `console.log` / `fetch` lands in the buffer instead of
    /// in the gap before the monitor arrives.
    ///
    /// The verdict is re-read here rather than carried from `start`: an `eval`
    /// deny that lands mid-session must stop the injection, not ride a grant the
    /// arming command made. The record is written only after Chrome confirms, so
    /// it never claims a registration the browser does not hold.
    pub(super) async fn ensure_monitor_hook(&self, kind: Monitor) -> Result<()> {
        let (armed, install_js) = match kind {
            Monitor::Console => (&self.console_monitoring, CONSOLE_INSTALL_JS),
            Monitor::Network => (&self.network_monitoring, NETWORK_INSTALL_JS),
        };
        let wanted = armed.load(std::sync::atomic::Ordering::Acquire)
            && crate::policy::enforce(&kind.start_command()).is_ok();
        let mut hooks = self.monitor_hooks.lock().await;
        match (wanted, hooks.get(&kind).cloned()) {
            (true, None) => {
                hooks.insert(kind, install_monitor_hook(&self.page, install_js).await?);
            }
            (false, Some(identifier)) => {
                self.page
                    .send(
                        "Page.removeScriptToEvaluateOnNewDocument",
                        Some(json!({ "identifier": identifier })),
                    )
                    .await?;
                hooks.remove(&kind);
            }
            _ => {}
        }
        Ok(())
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
        // exactly its host and a round-trip can't widen it to subdomains. A Secure
        // cookie partitioned under a FIRST-PARTY top-level site
        // (`has_cross_site_ancestor == false`) must take the PARTITION's scheme,
        // not the secure-implied `https`: Chrome validates CHIPS schemefully, so a
        // `https://…` set URL against an `http://…` partition (e.g.
        // `http://localhost`) is refused as "not first party". A cross-site
        // partition keeps the secure-implied scheme (its URL and top-level site
        // are different sites by construction). Mirrors browser-mode `state.js`.
        let scheme = c
            .partition_key
            .as_ref()
            .filter(|pk| !pk.has_cross_site_ancestor)
            .and_then(|pk| pk.top_level_site.split_once("://"))
            .map(|(s, _)| s)
            .filter(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'))
            })
            .unwrap_or(if c.secure { "https" } else { "http" });
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

/// The armed flag is set but THIS document carries no hook: an `eval` policy deny
/// removed the monitor's registration, so documents built after it carry nothing.
/// An empty success here would read as "the page was quiet" while the monitor was
/// in fact off.
const MONITOR_NOT_INSTALLED_CONSOLE: &str = "the console monitor is not installed in this document — an `eval` policy deny stops it being installed in a new document; check `webpilot policy list`, then run `webpilot console start`";
const MONITOR_NOT_INSTALLED_NETWORK: &str = "the network monitor is not installed in this document — an `eval` policy deny stops it being installed in a new document; check `webpilot policy list`, then run `webpilot network start`";

/// Register a monitor's install script to run in the MAIN world of every document
/// this page builds, the one already open included (`runImmediately`, so arming
/// needs no navigation). The MAIN world is where the page's own `console`/`fetch`
/// live — the bridge's isolated world would never observe them. Returns Chrome's
/// identifier for the registration, the handle a later policy deny removes it by.
async fn install_monitor_hook(page: &CdpSession, install_js: &str) -> Result<String> {
    let registered = page
        .send(
            "Page.addScriptToEvaluateOnNewDocument",
            Some(json!({
                "source": install_js,
                "runImmediately": true,
            })),
        )
        .await?;
    registered
        .get("identifier")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            WebPilotError::Other {
                detail: "Page.addScriptToEvaluateOnNewDocument returned no identifier".into(),
            }
            .into()
        })
}

/// The recorders both modes install, embedded from the extension that also
/// injects them — one source, so a change to what a monitor records cannot land
/// in one mode and miss the other.
const CONSOLE_INSTALL_JS: &str =
    include_str!("../../../../../extension/content/monitor-console.js");
const NETWORK_INSTALL_JS: &str =
    include_str!("../../../../../extension/content/monitor-network.js");

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
