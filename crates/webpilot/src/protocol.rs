use serde::{Deserialize, Serialize};

use crate::types::{DomSnapshot, TabInfo};

/// Request from CLI → Host → Extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u32,
    pub command: Command,
}

/// All commands the CLI can send.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Command {
    Capture {
        #[serde(default)]
        dom: bool,
        #[serde(default)]
        screenshot: bool,
        #[serde(default)]
        text: bool,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        bounds: bool,
        #[serde(default)]
        full_page: bool,
        #[serde(default)]
        accessibility: bool,
        #[serde(default)]
        occlusion: bool,
        #[serde(default)]
        annotate: bool,
    },
    Action {
        action: BrowserAction,
        #[serde(default)]
        capture: bool,
    },
    Evaluate {
        code: String,
    },
    Wait {
        #[serde(default)]
        selector: Option<String>,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        navigation: bool,
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
    },
    Status,
    ListTabs,
    SwitchTab {
        tab_id: u32,
    },
    NewTab {
        url: String,
    },
    CloseTab {
        tab_id: u32,
    },
    SetDom {
        selector: String,
        property: String,
        value: String,
        #[serde(default)]
        attr: Option<String>,
    },
    GetDom {
        selector: String,
        property: String,
        #[serde(default)]
        attr: Option<String>,
    },
    Fetch {
        url: String,
        #[serde(default)]
        method: Option<String>,
        #[serde(default)]
        body: Option<String>,
    },
    ListFrames,
    SwitchFrame {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        url_pattern: Option<String>,
        #[serde(default)]
        predicate: Option<String>,
        #[serde(default)]
        main: bool,
    },
    GetCookies {
        url: String,
    },
    SetCookie {
        url: String,
        name: String,
        value: String,
        #[serde(default)]
        http_only: bool,
        #[serde(default)]
        secure: bool,
    },
    DeleteCookie {
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
    ExportSession,
    ImportSession {
        data: String,
    },
    SetPolicy {
        action_type: String,
        verdict: String,
    },
    GetPolicies,
    ClearPolicies,
    Ping,
}

fn default_timeout() -> u64 {
    10000
}

/// Browser actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum BrowserAction {
    Click {
        index: u32,
    },
    Type {
        index: u32,
        text: String,
        #[serde(default)]
        clear: bool,
    },
    KeyPress {
        key: String,
        #[serde(default)]
        modifiers: Vec<String>,
    },
    Navigate {
        url: String,
    },
    GoBack,
    GoForward,
    Reload,
    ScrollDown {
        #[serde(default = "default_scroll")]
        amount: u32,
    },
    ScrollUp {
        #[serde(default = "default_scroll")]
        amount: u32,
    },
    ScrollToElement {
        index: u32,
    },
    Select {
        index: u32,
        value: String,
    },
    Hover {
        index: u32,
    },
    Focus {
        index: u32,
    },
    Upload {
        index: u32,
        path: String,
    },
}

fn default_scroll() -> u32 {
    600
}

/// Response from Extension → Host → CLI.
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
        page_url: String,
        page_title: String,
    },
    Action {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        dom: Option<DomSnapshot>,
        #[serde(skip_serializing_if = "Option::is_none")]
        url_changed: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        new_tab: Option<serde_json::Value>,
    },
    Evaluate {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    WaitResult {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    Status {
        connected: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        tab_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tab_title: Option<String>,
        extension_version: String,
    },
    Tabs {
        tabs: Vec<TabInfo>,
    },
    CommandResult {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    FetchResult {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Frames {
        frames: Vec<crate::types::FrameInfo>,
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
        error: Option<String>,
    },
    Cookies {
        cookies: Vec<crate::types::CookieInfo>,
    },
    CookieResult {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    ConsoleEntries {
        entries: Vec<crate::types::ConsoleEntry>,
    },
    NetworkLog {
        requests: Vec<crate::types::NetworkEntry>,
    },
    SessionExport {
        path: String,
    },
    SessionResult {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Policies {
        policies: Vec<crate::types::PolicyEntry>,
    },
    PolicyResult {
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Pong,
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}
