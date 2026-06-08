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
    ConsoleEntry, CookieInfo, DomSnapshot, FrameInfo, NetworkEntry, PolicyKey, TabInfo,
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

/// All command kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Command {
    Capture {
        #[serde(default)]
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
            Command::DomSet { .. } => Some(PolicyKey::DomSet),
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
    },
    NetworkEntries {
        entries: Vec<NetworkEntry>,
        /// As `ConsoleEntries::truncated`: the buffer is at its 500-entry cap, so
        /// older requests may have been evicted from this read.
        #[serde(default)]
        truncated: bool,
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
