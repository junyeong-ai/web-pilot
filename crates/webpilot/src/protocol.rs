//! Wire protocol between CLI client, host, and extension.
//!
//! All shapes derive Serialize/Deserialize so the same types are used on both
//! sides of the IPC boundary — there is no separate "wire" struct that drifts
//! from the in-process type.

use serde::{Deserialize, Serialize};

use crate::action::Action;
use crate::capture::{CaptureField, CaptureOpts};
use crate::error::WebPilotError;
use crate::types::{
    ConsoleEntry, CookieInfo, DomSnapshot, Download, FrameInfo, NetworkEntry, PolicyKey, SameSite,
    TabInfo,
};
use crate::wait::WaitCondition;

/// Wire envelope: monotonic id + command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u32,
    pub command: Command,
}

impl Request {
    pub fn new(id: u32, command: Command) -> Self {
        Self { id, command }
    }
}

/// `Capture` with no `include` defaults to the DOM — the most-used capture and a
/// sensible floor. An empty list yields a useless result (no DOM, no screenshot,
/// no error), so a wire caller that omits the field gets the DOM, matching the
/// CLI surface's own default.
fn default_capture_include() -> Vec<CaptureField> {
    vec![CaptureField::Dom]
}

/// All command kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Command {
    Capture {
        #[serde(default = "default_capture_include")]
        include: Vec<CaptureField>,
        #[serde(default)]
        opts: CaptureOpts,
        #[serde(default)]
        url: Option<String>,
    },
    Action {
        action: Action,
        #[serde(default)]
        capture: bool,
    },
    Eval {
        code: String,
    },
    Wait {
        condition: WaitCondition,
        #[serde(default = "default_wait_timeout_ms")]
        timeout_ms: u64,
    },
    Status,
    TabList,
    TabSwitch {
        tab_id: String,
    },
    TabNew {
        url: String,
    },
    TabClose {
        tab_id: String,
    },
    DomSet {
        selector: String,
        property: DomProperty,
        value: String,
    },
    DomGet {
        selector: String,
        property: DomProperty,
    },
    Fetch {
        url: String,
        #[serde(default)]
        method: Option<String>,
        #[serde(default)]
        body: Option<String>,
        /// Request headers as ordered `(name, value)` pairs — passed straight to
        /// `fetch`'s `headers` init. Empty by default: no content type is implied,
        /// so the caller controls it (a JSON body needs an explicit
        /// `content-type: application/json`). `credentials: include` is separate
        /// and always on — `fetch` runs as the page's authenticated session.
        #[serde(default)]
        headers: Vec<(String, String)>,
    },
    FrameList,
    FrameSwitch {
        selector: FrameSelector,
    },
    CookieList {
        url: String,
    },
    CookieSet {
        url: String,
        name: String,
        value: String,
        #[serde(default)]
        http_only: bool,
        #[serde(default)]
        secure: bool,
        /// SameSite attribute. Omitted leaves it off so Chrome applies its
        /// default — the read side (`cookie list`) reports it, so a set must be
        /// able to specify it for a faithful round-trip.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        same_site: Option<SameSite>,
        /// Absolute expiry as Unix-epoch seconds. Omitted = a session cookie.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires: Option<f64>,
    },
    CookieDelete {
        url: String,
        name: String,
    },
    ConsoleStart,
    ConsoleRead {
        /// Only entries with `timestamp >= since` (ms epoch) — the incremental
        /// cursor that lets an agent poll without re-reading or a destructive
        /// `console clear`. Same shape as `NetworkRead`.
        #[serde(default)]
        since: Option<u64>,
    },
    ConsoleClear,
    NetworkStart,
    NetworkRead {
        #[serde(default)]
        since: Option<u64>,
    },
    NetworkClear,
    SessionExport,
    SessionImport {
        data: String,
    },
    Ping,
}

impl Command {
    /// The policy key this command is gated by, if any. Drives the single
    /// enforcement point at the transport boundary. Read-only observation
    /// (capture, status, list/read commands) returns `None` — only operations
    /// that mutate page/browser state or move credentials are gated.
    pub fn policy_key(&self) -> Option<PolicyKey> {
        match self {
            Command::Action { action, .. } => Some(PolicyKey::from(action.kind())),
            Command::Eval { .. } => Some(PolicyKey::Eval),
            Command::Fetch { .. } => Some(PolicyKey::Fetch),
            // `set-html` assigns `innerHTML`: agent-supplied markup parsed into
            // the page, and an inline event-handler attribute in it runs JS
            // immediately — `<img src=x onerror=…>` fires on the failed load — a
            // direct, single-call JS-execution sink with the same effect as
            // `eval`. It is therefore gated by `eval`, not `dom_set`: denying
            // `eval` (the least-privilege base) must also deny this, or set-html
            // would reproduce arbitrary JS injection behind a narrower key.
            // `set-text` (textContent, literal) and `set-attr` (one attribute,
            // no immediate execution — an `on*` handler it sets still needs a
            // later click/navigate, themselves gated) stay `dom_set`.
            Command::DomSet { property, .. } => Some(match property {
                DomProperty::Html => PolicyKey::Eval,
                DomProperty::Text | DomProperty::Attr { .. } => PolicyKey::DomSet,
            }),
            Command::TabClose { .. } => Some(PolicyKey::TabClose),
            // `console start` / `network start` install monitoring hooks by
            // executing JS in the page's MAIN world — agent-initiated code
            // injection, gated by the same key as `eval`. Reading or clearing
            // the captured buffer afterwards is bookkeeping and stays ungated.
            Command::ConsoleStart | Command::NetworkStart => Some(PolicyKey::Eval),
            // Cookie reads return live session-cookie *values*, so the same
            // `cookie list` key gates both `cookie list` and `cookie get`.
            Command::CookieList { .. } => Some(PolicyKey::CookieList),
            Command::CookieSet { .. } => Some(PolicyKey::CookieSet),
            Command::CookieDelete { .. } => Some(PolicyKey::CookieDelete),
            Command::SessionExport => Some(PolicyKey::SessionExport),
            Command::SessionImport { .. } => Some(PolicyKey::SessionImport),
            // Navigation is keyed by effect, not command name: a `capture --url`
            // and `tab new URL` load a URL into a browsing context exactly as the
            // `navigate` action does, so all three sit behind `navigate`. A
            // URL-less capture only reads the current page and is not gated.
            Command::Capture { url: Some(_), .. } | Command::TabNew { .. } => {
                Some(PolicyKey::Navigate)
            }
            // A predicate runs caller-supplied script, so it sits behind the
            // same gate as `eval`; structural frame selectors do not.
            Command::FrameSwitch {
                selector: FrameSelector::Predicate { .. },
            } => Some(PolicyKey::Eval),
            // Read-only observation: no page/browser mutation, no credential
            // movement. Listed exhaustively (no wildcard) so a newly added
            // privileged command cannot silently default to ungated — the
            // compiler forces every variant to declare its gate.
            Command::Capture { url: None, .. }
            | Command::FrameSwitch { .. }
            | Command::Wait { .. }
            | Command::Status
            | Command::TabList
            | Command::TabSwitch { .. }
            | Command::DomGet { .. }
            | Command::FrameList
            | Command::ConsoleRead { .. }
            | Command::ConsoleClear
            | Command::NetworkRead { .. }
            | Command::NetworkClear
            | Command::Ping => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DomProperty {
    Html,
    Text,
    Attr { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum FrameSelector {
    Main,
    Name { value: String },
    Url { pattern: String },
    Predicate { js: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    Headless,
    Browser,
}

serde_plain::derive_display_from_serialize!(RunMode);

fn default_wait_timeout_ms() -> u64 {
    10_000
}

// ── Response ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u32,
    pub result: ResponseData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponseData {
    Capture {
        #[serde(skip_serializing_if = "Option::is_none")]
        dom: Option<DomSnapshot>,
        #[serde(skip_serializing_if = "Option::is_none")]
        screenshot_path: Option<String>,
        /// Saved image's pixel dimensions, and the downscale ratio applied
        /// when the capture exceeded the long-edge cap (`scale` present only
        /// then). Pixel coordinates measured on the saved image map back to
        /// page pixels via `coord / scale` — withholding the scale would make
        /// any coordinate math on a downscaled full-page shot silently wrong.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screenshot_width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screenshot_height: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screenshot_scale: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        screenshot_error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pdf_path: Option<String>,
        // Browser mode delivers raw bytes inline (the extension can't write
        // files); the CLI is the single writer and persists these to a path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pdf_b64: Option<String>,
        page_url: String,
        page_title: String,
        /// Files Chrome wrote to disk while this command ran. A navigation that
        /// resolves to an attachment leaves the page where it was, so without
        /// this the agent sees an unchanged snapshot and reads the command as a
        /// no-op — then retries, downloading the file again. Empty on every
        /// command that downloaded nothing.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        downloads: Vec<Download>,
    },
    Action {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<WebPilotError>,
        #[serde(skip_serializing_if = "Option::is_none")]
        dom: Option<DomSnapshot>,
        #[serde(skip_serializing_if = "Option::is_none")]
        url_changed: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        new_tab: Option<TabInfo>,
        /// Why the requested `--capture` snapshot is absent. The action itself
        /// succeeded — failing the whole command would invite a retry that
        /// re-runs the side effect (a double click), so the capture failure is
        /// reported alongside the success instead.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capture_error: Option<String>,
        /// Files Chrome wrote to disk while this command ran. A navigation that
        /// resolves to an attachment leaves the page where it was, so without
        /// this the agent sees an unchanged snapshot and reads the command as a
        /// no-op — then retries, downloading the file again. Empty on every
        /// command that downloaded nothing.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        downloads: Vec<Download>,
    },
    Eval {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<WebPilotError>,
    },
    Wait {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<WebPilotError>,
    },
    Status {
        connected: bool,
        mode: RunMode,
        #[serde(skip_serializing_if = "Option::is_none")]
        tab_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tab_title: Option<String>,
        /// Chrome version (always populated when `connected`).
        #[serde(skip_serializing_if = "Option::is_none")]
        chrome_version: Option<String>,
        /// WebPilot extension version (browser mode only).
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_version: Option<String>,
    },
    Tabs {
        tabs: Vec<TabInfo>,
    },
    CommandResult {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<WebPilotError>,
    },
    FetchResult {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<WebPilotError>,
    },
    Frames {
        frames: Vec<FrameInfo>,
        /// `None` when the main frame is active.
        #[serde(skip_serializing_if = "Option::is_none")]
        active_frame_id: Option<String>,
    },
    FrameSwitched {
        success: bool,
        /// `None` when switched back to the main frame.
        #[serde(skip_serializing_if = "Option::is_none")]
        frame_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<WebPilotError>,
    },
    Cookies {
        cookies: Vec<CookieInfo>,
    },
    CookieResult {
        success: bool,
        /// How many cookies a `cookie delete` actually removed — same-name
        /// cookies coexist across scopes (domain vs host-only, paths), so the
        /// count makes "all of them" verifiable. Absent for `cookie set`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deleted: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<WebPilotError>,
    },
    ConsoleEntries {
        entries: Vec<ConsoleEntry>,
        /// The monitor buffer is full (capped at 500), so older entries may have
        /// been evicted — an honest "this read may be incomplete" signal so a
        /// missing early entry never reads as a confident absence. Conservative:
        /// true whenever the buffer is at capacity, false otherwise.
        #[serde(default)]
        truncated: bool,
        /// Whether the recorder was in place before this document's first script,
        /// which is what decides whether an empty buffer means "the page reported
        /// nothing" or "nothing was watching". False for a document built while no
        /// WebPilot process was attached — one loaded between two CLI invocations,
        /// or a popup, which is already loading when its target appears — and
        /// always false in browser mode, which injects at navigation settle. The
        /// recorder stamps it from the null `documentElement` only a document-start
        /// injection sees; a missing field reads as false, so a peer that cannot
        /// answer never claims coverage it has not got.
        #[serde(default)]
        covers_load: bool,
    },
    NetworkEntries {
        entries: Vec<NetworkEntry>,
        /// As `ConsoleEntries::truncated`: the buffer is at its 500-entry cap, so
        /// older requests may have been evicted from this read.
        #[serde(default)]
        truncated: bool,
        /// As `ConsoleEntries::covers_load`: whether the recorder was in place
        /// before this document's first script, so a buffer with no requests in it
        /// is the page's silence rather than the recorder's absence.
        #[serde(default)]
        covers_load: bool,
    },
    SessionExport {
        path: String,
    },
    SessionResult {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<WebPilotError>,
    },
    Pong,
    Error {
        error: WebPilotError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_with_no_include_defaults_to_dom() {
        // A wire caller (raw IPC, or the host parsing a request) that omits
        // `include` must get the DOM, never a useless empty capture.
        let cmd: Command = serde_json::from_str(r#"{"type":"Capture"}"#).unwrap();
        let Command::Capture { include, .. } = cmd else {
            panic!("expected Capture, got {cmd:?}");
        };
        assert_eq!(include, vec![CaptureField::Dom]);
    }

    #[test]
    fn capture_respects_an_explicit_include() {
        let cmd: Command =
            serde_json::from_str(r#"{"type":"Capture","include":["screenshot"]}"#).unwrap();
        let Command::Capture { include, .. } = cmd else {
            panic!("expected Capture, got {cmd:?}");
        };
        assert_eq!(include, vec![CaptureField::Screenshot]);
    }
}
