//! Wire protocol between CLI client, host, and extension.
//!
//! All shapes derive Serialize/Deserialize so the same types are used on both
//! sides of the IPC boundary — there is no separate "wire" struct that drifts
//! from the in-process type.

use serde::{Deserialize, Serialize};

use crate::action::{Action, ActionKind};
use crate::capture::{CaptureField, CaptureOpts};
use crate::error::WebPilotError;
use crate::types::{
    ConsoleEntry, CookieInfo, DomSnapshot, FrameInfo, NetworkEntry, PolicyEntry, PolicyVerdict,
    TabInfo,
};
use crate::wait::WaitCondition;

pub const PROTOCOL_VERSION: u8 = 3;

/// Wire envelope: monotonic id + protocol version + command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u32,
    #[serde(default = "default_version")]
    pub version: u8,
    pub command: Command,
}

fn default_version() -> u8 {
    PROTOCOL_VERSION
}

impl Request {
    pub fn new(id: u32, command: Command) -> Self {
        Self {
            id,
            version: PROTOCOL_VERSION,
            command,
        }
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
    Evaluate {
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
    ConsoleRead,
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
    PolicySet {
        action: ActionKind,
        verdict: PolicyVerdict,
    },
    PolicyList,
    PolicyClear,
    Ping,
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

impl std::fmt::Display for RunMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Headless => "headless",
            Self::Browser => "browser",
        })
    }
}

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
    },
    Evaluate {
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
        active_frame_id: i64,
    },
    FrameSwitched {
        success: bool,
        frame_id: i64,
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
    },
    NetworkLog {
        requests: Vec<NetworkEntry>,
    },
    SessionExport {
        path: String,
    },
    SessionResult {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<WebPilotError>,
    },
    Policies {
        policies: Vec<PolicyEntry>,
    },
    PolicyResult {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<WebPilotError>,
    },
    Pong,
    Error {
        error: WebPilotError,
    },
}
