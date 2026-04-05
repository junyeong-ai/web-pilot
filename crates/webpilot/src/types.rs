use serde::{Deserialize, Serialize};

// ── Error types ──────────────────────────────────────────────────────────────

/// Machine-readable error codes for structured error handling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ErrorCode {
    // Element interaction
    ElementNotFound,
    SelectorNotFound,
    // Timing
    Timeout,
    // Navigation/page
    NavigationFailed,
    NoPage,
    FrameNotFound,
    // Input validation
    InvalidArgument,
    // Infrastructure
    BridgeUnavailable,
    ConnectionLost,
    // Security
    PolicyDenied,
    CSPViolation,
    // Session/context
    TabNotFound,
    ContextNotFound,
    SessionError,
    #[default]
    Unknown,
}

/// Error classification for retry logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// Re-capturable or retryable after DOM refresh
    Retryable,
    /// Caller must fix command arguments
    UserError,
    /// Infrastructure problem — reconnect or restart
    Infrastructure,
    /// Security policy — cannot bypass
    SecurityBlock,
    /// Unknown or unclassifiable
    Unknown,
}

impl ErrorCode {
    /// Classify this error for retry decisions.
    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::ElementNotFound | Self::SelectorNotFound | Self::Timeout
            | Self::NavigationFailed | Self::NoPage | Self::FrameNotFound => ErrorCategory::Retryable,
            Self::InvalidArgument | Self::TabNotFound | Self::ContextNotFound => {
                ErrorCategory::UserError
            }
            Self::BridgeUnavailable | Self::ConnectionLost | Self::SessionError => {
                ErrorCategory::Infrastructure
            }
            Self::PolicyDenied | Self::CSPViolation => ErrorCategory::SecurityBlock,
            Self::Unknown => ErrorCategory::Unknown,
        }
    }

    /// Whether the caller should retry this operation.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.category(),
            ErrorCategory::Retryable | ErrorCategory::Infrastructure
        )
    }

    /// Map to CLI exit code.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::ElementNotFound | Self::SelectorNotFound | Self::TabNotFound
            | Self::ContextNotFound | Self::FrameNotFound => 4,
            Self::Timeout => 5,
            Self::PolicyDenied | Self::CSPViolation => 6,
            Self::ConnectionLost | Self::BridgeUnavailable => 3,
            Self::InvalidArgument => 7,
            Self::NavigationFailed | Self::NoPage => 8,
            Self::SessionError | Self::Unknown => 1,
        }
    }

    /// Parse from string (case-insensitive), for bridge.js PascalCase codes.
    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "elementnotfound" => Self::ElementNotFound,
            "selectornotfound" => Self::SelectorNotFound,
            "timeout" => Self::Timeout,
            "navigationfailed" => Self::NavigationFailed,
            "nopage" => Self::NoPage,
            "framenotfound" => Self::FrameNotFound,
            "invalidargument" => Self::InvalidArgument,
            "bridgeunavailable" => Self::BridgeUnavailable,
            "connectionlost" => Self::ConnectionLost,
            "policydenied" => Self::PolicyDenied,
            "cspviolation" => Self::CSPViolation,
            "tabnotfound" => Self::TabNotFound,
            "contextnotfound" => Self::ContextNotFound,
            "sessionerror" => Self::SessionError,
            _ => Self::Unknown,
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ElementNotFound => write!(f, "ElementNotFound"),
            Self::Timeout => write!(f, "Timeout"),
            Self::PolicyDenied => write!(f, "PolicyDenied"),
            Self::NoPage => write!(f, "NoPage"),
            Self::NavigationFailed => write!(f, "NavigationFailed"),
            Self::FrameNotFound => write!(f, "FrameNotFound"),
            Self::SelectorNotFound => write!(f, "SelectorNotFound"),
            Self::InvalidArgument => write!(f, "InvalidArgument"),
            Self::BridgeUnavailable => write!(f, "BridgeUnavailable"),
            Self::ConnectionLost => write!(f, "ConnectionLost"),
            Self::CSPViolation => write!(f, "CSPViolation"),
            Self::TabNotFound => write!(f, "TabNotFound"),
            Self::ContextNotFound => write!(f, "ContextNotFound"),
            Self::SessionError => write!(f, "SessionError"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

/// Unified protocol error with human-readable message and machine-readable code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolError {
    pub message: String,
    pub code: ErrorCode,
}

/// Structured error type for CLI exit code propagation.
#[derive(Debug)]
pub struct WebPilotError {
    pub code: ErrorCode,
    pub message: String,
}

impl std::fmt::Display for WebPilotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WebPilotError {}

impl From<ProtocolError> for WebPilotError {
    fn from(e: ProtocolError) -> Self {
        Self {
            code: e.code,
            message: e.message,
        }
    }
}

// ── Policy types ─────────────────────────────────────────────────────────────

/// Browser action type for policy rules.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ActionType {
    Click,
    Type,
    KeyPress,
    Navigate,
    Back,
    Forward,
    Reload,
    ScrollDown,
    ScrollUp,
    ScrollToElement,
    Select,
    Hover,
    Focus,
    Upload,
    Drag,
}

/// Policy verdict: allow or deny an action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PolicyVerdict {
    Allow,
    Deny,
}

impl std::fmt::Display for ActionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Click => write!(f, "click"),
            Self::Type => write!(f, "type"),
            Self::KeyPress => write!(f, "keypress"),
            Self::Navigate => write!(f, "navigate"),
            Self::Back => write!(f, "back"),
            Self::Forward => write!(f, "forward"),
            Self::Reload => write!(f, "reload"),
            Self::ScrollDown => write!(f, "scrolldown"),
            Self::ScrollUp => write!(f, "scrollup"),
            Self::ScrollToElement => write!(f, "scrolltoelement"),
            Self::Select => write!(f, "select"),
            Self::Hover => write!(f, "hover"),
            Self::Focus => write!(f, "focus"),
            Self::Upload => write!(f, "upload"),
            Self::Drag => write!(f, "drag"),
        }
    }
}

impl std::fmt::Display for PolicyVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Allow => write!(f, "allow"),
            Self::Deny => write!(f, "deny"),
        }
    }
}

impl From<&crate::protocol::BrowserAction> for ActionType {
    fn from(action: &crate::protocol::BrowserAction) -> Self {
        use crate::protocol::BrowserAction;
        match action {
            BrowserAction::Click { .. } => Self::Click,
            BrowserAction::Type { .. } => Self::Type,
            BrowserAction::KeyPress { .. } => Self::KeyPress,
            BrowserAction::Navigate { .. } => Self::Navigate,
            BrowserAction::Back => Self::Back,
            BrowserAction::Forward => Self::Forward,
            BrowserAction::Reload => Self::Reload,
            BrowserAction::ScrollDown { .. } => Self::ScrollDown,
            BrowserAction::ScrollUp { .. } => Self::ScrollUp,
            BrowserAction::ScrollToElement { .. } => Self::ScrollToElement,
            BrowserAction::Select { .. } => Self::Select,
            BrowserAction::Hover { .. } => Self::Hover,
            BrowserAction::Focus { .. } => Self::Focus,
            BrowserAction::Upload { .. } => Self::Upload,
            BrowserAction::Drag { .. } => Self::Drag,
        }
    }
}

/// Action policy entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyEntry {
    pub action_type: ActionType,
    pub verdict: PolicyVerdict,
}

// ── Console types ────────────────────────────────────────────────────────────

/// Console log level.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleLevel {
    Log,
    Error,
    Warn,
    Info,
    Debug,
}

impl std::fmt::Display for ConsoleLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Log => write!(f, "log"),
            Self::Error => write!(f, "error"),
            Self::Warn => write!(f, "warn"),
            Self::Info => write!(f, "info"),
            Self::Debug => write!(f, "debug"),
        }
    }
}

impl ConsoleLevel {
    /// Parse a string into a ConsoleLevel, case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "log" => Some(Self::Log),
            "error" => Some(Self::Error),
            "warn" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            _ => None,
        }
    }
}

/// Console log entry captured from the page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleEntry {
    pub level: ConsoleLevel,
    pub message: String,
    pub timestamp: u64,
}

// ── Cookie types ─────────────────────────────────────────────────────────────

/// SameSite cookie attribute.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SameSite {
    Strict,
    Lax,
    /// Chrome's cookies API uses "no_restriction" for SameSite=None.
    #[serde(alias = "no_restriction")]
    None,
    Unspecified,
}

/// Cookie information returned by chrome.cookies API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookieInfo {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
    pub same_site: SameSite,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration: Option<f64>,
}

// ── DOM types ────────────────────────────────────────────────────────────────

/// An interactive element extracted from the page DOM.
/// Split into logical sub-groups for maintainability; JSON shape unchanged via `#[serde(flatten)]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractiveElement {
    // Identity
    pub index: u32,
    pub tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub text: String,

    // Semantic attributes
    #[serde(flatten)]
    pub semantics: ElementSemantics,

    // Form/interaction state
    #[serde(flatten)]
    pub state: ElementState,

    // Spatial/visibility info
    #[serde(flatten)]
    pub spatial: ElementSpatial,
}

/// Semantic attributes of an interactive element.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ElementSemantics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autocomplete: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub form_id: Option<String>,
}

/// Form/interaction state of an interactive element.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ElementState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_string_or_bool"
    )]
    pub expanded: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_string_or_bool"
    )]
    pub selected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readonly: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<SelectOption>>,
}

/// Spatial/visibility information of an interactive element.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ElementSpatial {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_viewport: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occluded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landmark: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_new: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: String,
    pub text: String,
    #[serde(default)]
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Frame information for iframe navigation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameInfo {
    pub frame_id: i64,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_frame_id: Option<i64>,
    pub is_main: bool,
}

/// Network request entry captured by fetch/XHR interception.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkEntry {
    #[serde(rename = "type")]
    pub req_type: String,
    pub url: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub duration_ms: f64,
    pub timestamp: u64,
}

// ── Snapshot types ───────────────────────────────────────────────────────────

/// Snapshot of a page's interactive state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomSnapshot {
    pub elements: Vec<InteractiveElement>,
    pub total_nodes: u32,
    pub page_url: String,
    pub page_title: String,
    pub scroll: ScrollInfo,
    #[serde(default)]
    pub scroll_percent: u32,
    pub extraction_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessibility_tree: Option<String>,
}

/// Scroll position information.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ScrollInfo {
    pub scroll_x: f64,
    pub scroll_y: f64,
    pub scroll_width: f64,
    pub scroll_height: f64,
    pub viewport_width: f64,
    pub viewport_height: f64,
}

impl ScrollInfo {
    pub fn pages_above(&self) -> f64 {
        if self.viewport_height > 0.0 {
            self.scroll_y / self.viewport_height
        } else {
            0.0
        }
    }

    pub fn pages_below(&self) -> f64 {
        if self.viewport_height > 0.0 {
            ((self.scroll_height - self.scroll_y - self.viewport_height).max(0.0))
                / self.viewport_height
        } else {
            0.0
        }
    }
}

// ── Tab types ────────────────────────────────────────────────────────────────

/// Tab info for tab listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabInfo {
    #[serde(deserialize_with = "deserialize_id_as_string")]
    pub id: String,
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub active: bool,
}

/// Accept both integer and string as tab ID (Chrome sends integer, CDP sends string).
fn deserialize_id_as_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct IdVisitor;
    impl<'de> de::Visitor<'de> for IdVisitor {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string or integer")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_string<E: de::Error>(self, v: String) -> Result<String, E> {
            Ok(v)
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<String, E> {
            Ok(v.to_string())
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<String, E> {
            Ok(v.to_string())
        }
    }
    deserializer.deserialize_any(IdVisitor)
}

/// Deserialize a value that may be a boolean or a string "true"/"false" into Option<bool>.
/// Handles: true, false, "true", "false", null/missing → None.
fn deserialize_string_or_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct StringOrBoolVisitor;
    impl<'de> de::Visitor<'de> for StringOrBoolVisitor {
        type Value = Option<bool>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a boolean, \"true\"/\"false\" string, or null")
        }
        fn visit_bool<E: de::Error>(self, v: bool) -> Result<Option<bool>, E> {
            Ok(Some(v))
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<Option<bool>, E> {
            match v {
                "true" => Ok(Some(true)),
                "false" => Ok(Some(false)),
                _ => Ok(None),
            }
        }
        fn visit_none<E: de::Error>(self) -> Result<Option<bool>, E> {
            Ok(None)
        }
        fn visit_unit<E: de::Error>(self) -> Result<Option<bool>, E> {
            Ok(None)
        }
    }
    deserializer.deserialize_any(StringOrBoolVisitor)
}

// ── Filter types ─────────────────────────────────────────────────────────────

/// Filter criteria for semantic element search.
#[derive(Debug, Default)]
pub struct ElementFilter {
    pub role: Option<String>,
    pub text: Option<String>,
    pub label: Option<String>,
    pub placeholder: Option<String>,
    pub tag: Option<String>,
}

impl InteractiveElement {
    /// Get the implicit ARIA role based on tag name and input type.
    pub fn implicit_role(&self) -> Option<&'static str> {
        match (self.tag.as_str(), self.semantics.input_type.as_deref()) {
            ("a", _) if self.semantics.href.is_some() => Some("link"),
            ("button", _) => Some("button"),
            ("input", Some("text" | "search" | "email" | "url" | "tel")) => Some("textbox"),
            ("input", Some("checkbox")) => Some("checkbox"),
            ("input", Some("radio")) => Some("radio"),
            ("input", Some("number")) => Some("spinbutton"),
            ("input", Some("range")) => Some("slider"),
            ("select", _) => Some("combobox"),
            ("textarea", _) => Some("textbox"),
            ("img", _) => Some("img"),
            ("nav", _) => Some("navigation"),
            ("main", _) => Some("main"),
            ("header", _) => Some("banner"),
            ("footer", _) => Some("contentinfo"),
            ("aside", _) => Some("complementary"),
            ("form", _) => Some("form"),
            _ => None,
        }
    }

    /// Check if this element matches the given filter criteria (AND logic).
    pub fn matches(&self, filter: &ElementFilter) -> bool {
        if let Some(ref role) = filter.role {
            let role_lower = role.to_lowercase();
            let explicit_match = self
                .role
                .as_ref()
                .map(|r| r.to_lowercase() == role_lower)
                .unwrap_or(false);
            let implicit_match = self
                .implicit_role()
                .map(|r| r == role_lower)
                .unwrap_or(false);
            let tag_match = self.tag.to_lowercase() == role_lower;
            if !explicit_match && !implicit_match && !tag_match {
                return false;
            }
        }
        if let Some(ref text) = filter.text {
            let text_lower = text.to_lowercase();
            let in_text = self.text.to_lowercase().contains(&text_lower);
            let in_name = self
                .semantics
                .name
                .as_ref()
                .map(|n| n.to_lowercase().contains(&text_lower))
                .unwrap_or(false);
            if !in_text && !in_name {
                return false;
            }
        }
        if let Some(ref label) = filter.label {
            let label_lower = label.to_lowercase();
            if !self
                .semantics
                .label
                .as_ref()
                .map(|l| l.to_lowercase().contains(&label_lower))
                .unwrap_or(false)
            {
                return false;
            }
        }
        if let Some(ref ph) = filter.placeholder {
            let ph_lower = ph.to_lowercase();
            if !self
                .semantics
                .placeholder
                .as_ref()
                .map(|p| p.to_lowercase().contains(&ph_lower))
                .unwrap_or(false)
            {
                return false;
            }
        }
        if let Some(ref tag) = filter.tag
            && self.tag.to_lowercase() != tag.to_lowercase()
        {
            return false;
        }
        true
    }
}

// ── DOM serialization ────────────────────────────────────────────────────────

impl DomSnapshot {
    /// Serialize to LLM-friendly text format.
    pub fn to_text(&self) -> String {
        let mut out = String::with_capacity(self.elements.len() * 80);

        for el in &self.elements {
            let new_marker = if el.spatial.is_new == Some(true) { "*" } else { "" };

            let tag_id = if let Some(ref id) = el.id {
                format!("{}#{id}", el.tag)
            } else {
                el.tag.clone()
            };
            out.push_str(&format!("{new_marker}[{}] {tag_id} ", el.index));

            if let Some(ref role) = el.role
                && role != &el.tag
            {
                out.push_str(&format!("role={role} "));
            }

            if !el.text.is_empty() {
                out.push_str(&format!("\"{}\" ", el.text));
            } else if let Some(ref name) = el.semantics.name {
                out.push_str(&format!("\"{}\" ", name));
            }

            if let Some(ref label) = el.semantics.label {
                out.push_str(&format!("label=\"{label}\" "));
            }
            if let Some(ref ph) = el.semantics.placeholder
                && ph != &el.text
            {
                out.push_str(&format!("placeholder=\"{ph}\" "));
            }
            if let Some(ref href) = el.semantics.href {
                if href.len() > 50 {
                    out.push_str(&format!("href=\"{}...\" ", &href[..50]));
                } else {
                    out.push_str(&format!("href=\"{href}\" "));
                }
            }
            if let Some(ref val) = el.state.value
                && !val.is_empty()
            {
                out.push_str(&format!("value=\"{val}\" "));
            }
            if let Some(ref it) = el.semantics.input_type {
                out.push_str(&format!("type={it} "));
            }
            if let Some(ref ac) = el.semantics.autocomplete {
                out.push_str(&format!("autocomplete={ac} "));
            }
            if let Some(true) = el.state.checked {
                out.push_str("[checked] ");
            }
            if el.state.expanded == Some(true) {
                out.push_str("[expanded] ");
            }
            if el.state.selected == Some(true) {
                out.push_str("[selected] ");
            }
            if let Some(true) = el.state.required {
                out.push_str("[required] ");
            }
            if let Some(true) = el.state.readonly {
                out.push_str("[readonly] ");
            }
            if el.state.disabled {
                out.push_str("[disabled] ");
            }
            if el.state.focused {
                out.push_str("[focused] ");
            }
            if let Some(ref opts) = el.state.options {
                let sel = opts
                    .iter()
                    .find(|o| o.selected)
                    .map(|o| o.text.as_str())
                    .unwrap_or("");
                let count = opts.len();
                out.push_str(&format!("options({count}) selected=\"{sel}\" "));
            }
            if let Some(true) = el.spatial.occluded {
                out.push_str("[occluded] ");
            }
            if let Some(false) = el.spatial.in_viewport {
                out.push_str("[offscreen] ");
            }
            if let Some(ref desc) = el.semantics.description {
                out.push_str(&format!("description=\"{desc}\" "));
            }
            if let Some(ref form) = el.semantics.form_id {
                out.push_str(&format!("form={form} "));
            }
            if let Some(ref frame) = el.spatial.frame {
                out.push_str(&format!("frame={frame} "));
            }
            if let Some(ref lm) = el.spatial.landmark {
                out.push_str(&format!("@{lm} "));
            }

            out.push('\n');
        }

        // Footer
        out.push_str(&format!(
            "--- Page: {} ({}) ---\n",
            self.page_title, self.page_url
        ));

        let scroll = &self.scroll;
        let above = scroll.pages_above();
        let below = scroll.pages_below();
        let pct = self.scroll_percent;
        if above < 0.05 && below < 0.05 {
            out.push_str("--- Scroll: entire page visible ---\n");
        } else {
            out.push_str(&format!(
                "--- Scroll: {pct}% ({above:.1} above, {below:.1} below) ---\n"
            ));
        }

        out.push_str(&format!(
            "--- {} elements (from {} nodes, {}ms) ---\n",
            self.elements.len(),
            self.total_nodes,
            self.extraction_ms,
        ));

        out
    }
}

