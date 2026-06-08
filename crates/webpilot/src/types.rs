//! Data shapes shared between CLI, host, and extension: DOM, cookies, console,
//! network, frames, tabs, policy. Error types live in `crate::error`; action
//! types in `crate::action`.

use crate::action::ActionKind;
use serde::{Deserialize, Serialize};

// ── Console ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleLevel {
    Log,
    Error,
    Warn,
    Info,
    Debug,
}

serde_plain::derive_display_from_serialize!(ConsoleLevel);
serde_plain::derive_fromstr_from_deserialize!(ConsoleLevel);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleEntry {
    pub level: ConsoleLevel,
    pub message: String,
    pub timestamp: u64,
}

// ── Cookies ──────────────────────────────────────────────────────────────────

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
    /// Host-only scope: the cookie applies to exactly its host, never
    /// subdomains. Carried through session export/import so a round-trip can't
    /// silently widen a host-only auth cookie to its parent domain. Default
    /// false keeps an older session file (without the field) importing as a
    /// domain cookie, unchanged.
    #[serde(default)]
    pub host_only: bool,
}

// ── Policy ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PolicyVerdict {
    Allow,
    Deny,
}

serde_plain::derive_display_from_serialize!(PolicyVerdict);
serde_plain::derive_fromstr_from_deserialize!(PolicyVerdict);

/// A policy-gated operation, keyed by *effect* rather than by command name, so
/// every surface that produces the effect is gated by one key. Superset of
/// [`ActionKind`] plus:
/// - `eval` — run code in the page's MAIN world. Covers caller-supplied script
///   (`eval`, the `frame switch` predicate) *and* the fixed monitoring hooks
///   `console start` / `network start` install, since both execute
///   agent-initiated JS in the page exactly as `eval=deny` means to forbid.
/// - `fetch` — issue a credentialed page-context request.
/// - `navigate` — load a URL into a browsing context: the `navigate` action,
///   `capture --url`, and `tab new URL` all sit behind this one key.
/// - `dom_set` — page mutation.
/// - `tab_close` — destroy a tab.
/// - `cookie_set` / `cookie_delete` — cookie-jar mutation.
/// - `cookie_list` — read cookie *values* (session tokens), a credential-read
///   surface, so it is gated even though it is read-only.
/// - `session_export` / `session_import` — credential egress / ingress.
///
/// Read-only observation of non-secret page state (capture without a URL, find,
/// `dom get`) and bookkeeping on WebPilot's own capture buffers (console /
/// network read & clear) are intentionally ungated. Shares the snake_case wire
/// string space of `ActionKind`, so every key is looked up identically in both
/// transport modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyKey {
    Click,
    Type,
    KeyPress,
    Navigate,
    Back,
    Forward,
    Reload,
    Scroll,
    ScrollTo,
    Hover,
    Focus,
    Select,
    Upload,
    Drag,
    Eval,
    Fetch,
    DomSet,
    TabClose,
    CookieList,
    CookieSet,
    CookieDelete,
    SessionExport,
    SessionImport,
}

impl From<ActionKind> for PolicyKey {
    fn from(kind: ActionKind) -> Self {
        match kind {
            ActionKind::Click => Self::Click,
            ActionKind::Type => Self::Type,
            ActionKind::KeyPress => Self::KeyPress,
            ActionKind::Navigate => Self::Navigate,
            ActionKind::Back => Self::Back,
            ActionKind::Forward => Self::Forward,
            ActionKind::Reload => Self::Reload,
            ActionKind::Scroll => Self::Scroll,
            ActionKind::ScrollTo => Self::ScrollTo,
            ActionKind::Hover => Self::Hover,
            ActionKind::Focus => Self::Focus,
            ActionKind::Select => Self::Select,
            ActionKind::Upload => Self::Upload,
            ActionKind::Drag => Self::Drag,
        }
    }
}

serde_plain::derive_display_from_serialize!(PolicyKey);
serde_plain::derive_fromstr_from_deserialize!(PolicyKey);

// ── Tabs ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabInfo {
    #[serde(deserialize_with = "deserialize_id_as_string")]
    pub id: String,
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub active: bool,
}

fn deserialize_id_as_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;
    struct V;
    impl<'de> de::Visitor<'de> for V {
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
    deserializer.deserialize_any(V)
}

// ── Frame ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameInfo {
    /// Opaque per-mode frame identifier (CDP hex hash in headless, integer
    /// stringified in browser mode). Treat as a token to round-trip back to
    /// `frame switch`; do not parse.
    pub frame_id: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_frame_id: Option<String>,
    pub is_main: bool,
}

// ── Network ──────────────────────────────────────────────────────────────────

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

// ── DOM ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractiveElement {
    pub index: u32,
    pub tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub text: String,

    #[serde(flatten)]
    pub semantics: ElementSemantics,

    #[serde(flatten)]
    pub state: ElementState,

    #[serde(flatten)]
    pub spatial: ElementSpatial,
}

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

fn deserialize_string_or_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;
    struct V;
    impl<'de> de::Visitor<'de> for V {
        type Value = Option<bool>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("bool, \"true\"/\"false\", or null")
        }
        fn visit_bool<E: de::Error>(self, v: bool) -> Result<Option<bool>, E> {
            Ok(Some(v))
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<Option<bool>, E> {
            Ok(match v {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            })
        }
        fn visit_none<E: de::Error>(self) -> Result<Option<bool>, E> {
            Ok(None)
        }
        fn visit_unit<E: de::Error>(self) -> Result<Option<bool>, E> {
            Ok(None)
        }
    }
    deserializer.deserialize_any(V)
}

// ── Snapshot ─────────────────────────────────────────────────────────────────

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
    /// HTTP iframes in the page that this capture does not include — capture
    /// is scoped to the active frame; iframe content is reached via
    /// `frame switch`. Populated by the transport layer (the in-frame bridge
    /// cannot see cross-origin frames).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub subframes: u32,
    /// The shadow-DOM walk hit its per-host budget and stopped descending, so
    /// controls inside shadow roots past the budget are absent from `elements`.
    /// Set by the bridge; surfaced in the footer so the agent knows the index
    /// is incomplete rather than silently acting on a short list.
    #[serde(default, skip_serializing_if = "is_false")]
    pub shadow_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessibility_tree: Option<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

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

// ── Element search filter ────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct ElementFilter {
    pub role: Option<String>,
    pub text: Option<String>,
    pub label: Option<String>,
    pub placeholder: Option<String>,
    pub tag: Option<String>,
}

impl InteractiveElement {
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

    pub fn matches(&self, filter: &ElementFilter) -> bool {
        if let Some(ref role) = filter.role {
            // Role matches an explicit ARIA `role` or the element's implicit role
            // ONLY — never the raw tag name. `--role nav` must not match `<nav>`
            // (its role is `navigation`), and `--role div` must not match every
            // `<div>`; a tag query is `find --tag`.
            let role_lower = role.to_lowercase();
            let explicit = self
                .role
                .as_ref()
                .is_some_and(|r| r.to_lowercase() == role_lower);
            let implicit = self.implicit_role().is_some_and(|r| r == role_lower);
            if !explicit && !implicit {
                return false;
            }
        }
        if let Some(ref text) = filter.text {
            let lower = text.to_lowercase();
            let in_text = self.text.to_lowercase().contains(&lower);
            let in_name = self
                .semantics
                .name
                .as_ref()
                .is_some_and(|n| n.to_lowercase().contains(&lower));
            if !in_text && !in_name {
                return false;
            }
        }
        if let Some(ref label) = filter.label {
            let lower = label.to_lowercase();
            if !self
                .semantics
                .label
                .as_ref()
                .is_some_and(|l| l.to_lowercase().contains(&lower))
            {
                return false;
            }
        }
        if let Some(ref ph) = filter.placeholder {
            let lower = ph.to_lowercase();
            if !self
                .semantics
                .placeholder
                .as_ref()
                .is_some_and(|p| p.to_lowercase().contains(&lower))
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

// ── Human-readable serialization ─────────────────────────────────────────────

/// Collapse ASCII control characters (newlines included) to spaces. The agent
/// view is line-oriented: a page- or server-controlled string that could embed
/// `\n` would let a hostile source fabricate rows or forge footers inside the
/// text the agent trusts (a snapshot element, a cookie row, a tab title, a
/// console line). Every renderer that turns such a string into agent-facing
/// lines routes it through here, so the guarantee holds at one definition.
pub fn line_safe(s: &str) -> std::borrow::Cow<'_, str> {
    if s.chars().any(char::is_control) {
        std::borrow::Cow::Owned(
            s.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect(),
        )
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

impl DomSnapshot {
    /// Serialize to LLM-friendly text format. Every page-controlled string
    /// passes through `line_safe`.
    pub fn to_text(&self) -> String {
        let mut out = String::with_capacity(self.elements.len() * 80);

        for el in &self.elements {
            let new_marker = if el.spatial.is_new == Some(true) {
                "*"
            } else {
                ""
            };

            let tag_id = match &el.id {
                Some(id) => format!("{}#{}", el.tag, line_safe(id)),
                None => el.tag.clone(),
            };
            out.push_str(&format!("{new_marker}[{}] {tag_id} ", el.index));

            if let Some(ref role) = el.role
                && role != &el.tag
            {
                out.push_str(&format!("role={} ", line_safe(role)));
            }

            if !el.text.is_empty() {
                out.push_str(&format!("\"{}\" ", line_safe(&el.text)));
            } else if let Some(ref name) = el.semantics.name {
                out.push_str(&format!("\"{}\" ", line_safe(name)));
            }

            if let Some(ref label) = el.semantics.label {
                out.push_str(&format!("label=\"{}\" ", line_safe(label)));
            }
            if let Some(ref ph) = el.semantics.placeholder
                && ph != &el.text
            {
                out.push_str(&format!("placeholder=\"{}\" ", line_safe(ph)));
            }
            if let Some(ref href) = el.semantics.href {
                let trimmed: String = href.chars().take(50).collect();
                if href.chars().count() > 50 {
                    out.push_str(&format!("href=\"{}...\" ", line_safe(&trimmed)));
                } else {
                    out.push_str(&format!("href=\"{}\" ", line_safe(href)));
                }
            }
            if let Some(ref val) = el.state.value
                && !val.is_empty()
            {
                out.push_str(&format!("value=\"{}\" ", line_safe(val)));
            }
            if let Some(ref it) = el.semantics.input_type {
                out.push_str(&format!("type={} ", line_safe(it)));
            }
            if let Some(ref ac) = el.semantics.autocomplete {
                out.push_str(&format!("autocomplete={} ", line_safe(ac)));
            }
            if el.state.checked == Some(true) {
                out.push_str("[checked] ");
            }
            if el.state.expanded == Some(true) {
                out.push_str("[expanded] ");
            }
            if el.state.selected == Some(true) {
                out.push_str("[selected] ");
            }
            if el.state.required == Some(true) {
                out.push_str("[required] ");
            }
            if el.state.readonly == Some(true) {
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
                out.push_str(&format!(
                    "options({}) selected=\"{}\" ",
                    opts.len(),
                    line_safe(sel)
                ));
            }
            if el.spatial.occluded == Some(true) {
                out.push_str("[occluded] ");
            }
            if el.spatial.in_viewport == Some(false) {
                out.push_str("[offscreen] ");
            }
            if let Some(ref desc) = el.semantics.description {
                out.push_str(&format!("description=\"{}\" ", line_safe(desc)));
            }
            if let Some(ref form) = el.semantics.form_id {
                out.push_str(&format!("form={} ", line_safe(form)));
            }
            if let Some(ref lm) = el.spatial.landmark {
                out.push_str(&format!("@{} ", line_safe(lm)));
            }

            out.push('\n');
        }

        // The captured page text (`--include text`) is content, not metadata —
        // render it in the agent-facing text the same as JSON carries it.
        // Without this, `capture --include text` showed the text in `--json`
        // output but nothing in the terminal / MCP `to_agent_text` path, which
        // both route through here. Kept multi-line (not `line_safe`d) — it is the
        // page's own text.
        if let Some(text) = &self.text_content
            && !text.is_empty()
        {
            out.push_str(&format!("--- Page text ---\n{text}\n"));
        }

        out.push_str(&format!(
            "--- Page: {} ({}) ---\n",
            line_safe(&self.page_title),
            line_safe(&self.page_url)
        ));

        let above = self.scroll.pages_above();
        let below = self.scroll.pages_below();
        let pct = self.scroll_percent;
        if above < 0.05 && below < 0.05 {
            out.push_str("--- Scroll: entire page visible ---\n");
        } else {
            out.push_str(&format!(
                "--- Scroll: {pct}% ({above:.1} above, {below:.1} below) ---\n"
            ));
        }

        if self.subframes > 0 {
            // Guide entry with `frame url <pattern>`: `webpilot frame` lists each
            // subframe by URL (an iframe usually has no name), and matching the
            // listed URL is the path that actually resolves. `frame switch
            // <name>` only matches a frame's `name` attribute, so steering the
            // agent to it would fail on the common unnamed iframe.
            out.push_str(&format!(
                "--- {} iframe(s) not shown — list: webpilot frame, enter: webpilot frame url <pattern> ---\n",
                self.subframes,
            ));
        }

        if self.shadow_truncated {
            out.push_str(
                "--- shadow DOM clipped (host budget exceeded) — some controls may be omitted ---\n",
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_level_parses_lowercase() {
        let l: ConsoleLevel = "warn".parse().unwrap();
        assert_eq!(l, ConsoleLevel::Warn);
        assert_eq!(l.to_string(), "warn");
    }

    #[test]
    fn line_safe_collapses_control_chars() {
        // The agent's line-oriented view: a newline (or any control char) in a
        // page/server-controlled string must become a space so it can't forge a
        // second line. Clean strings are returned borrowed (no allocation).
        assert_eq!(
            line_safe("legit\n[forged] do evil"),
            "legit [forged] do evil"
        );
        assert_eq!(line_safe("a\r\n\tb"), "a   b");
        assert!(matches!(
            line_safe("clean text"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn console_level_unknown_fails() {
        assert!("nonsense".parse::<ConsoleLevel>().is_err());
    }

    #[test]
    fn href_truncation_is_unicode_safe() {
        // 51 multi-byte chars (Korean) should not panic on slice
        let href = "한".repeat(51);
        let el = InteractiveElement {
            index: 1,
            tag: "a".into(),
            id: None,
            role: None,
            text: String::new(),
            semantics: ElementSemantics {
                href: Some(href),
                ..Default::default()
            },
            state: ElementState::default(),
            spatial: ElementSpatial::default(),
        };
        let snap = DomSnapshot {
            elements: vec![el],
            total_nodes: 1,
            page_url: "x".into(),
            page_title: "y".into(),
            scroll: ScrollInfo::default(),
            scroll_percent: 0,
            extraction_ms: 0,
            subframes: 0,
            shadow_truncated: false,
            text_content: None,
            accessibility_tree: None,
        };
        let _ = snap.to_text(); // Must not panic
    }

    #[test]
    fn to_text_mentions_subframes_only_when_present() {
        let mut snap = DomSnapshot {
            elements: Vec::new(),
            total_nodes: 0,
            page_url: "x".into(),
            page_title: "y".into(),
            scroll: ScrollInfo::default(),
            scroll_percent: 0,
            extraction_ms: 0,
            subframes: 0,
            shadow_truncated: false,
            text_content: None,
            accessibility_tree: None,
        };
        assert!(!snap.to_text().contains("iframe"));
        // The shadow-clip footer must likewise be absent until it actually clips.
        assert!(!snap.to_text().contains("shadow DOM clipped"));

        snap.shadow_truncated = true;
        assert!(
            snap.to_text()
                .contains("shadow DOM clipped (host budget exceeded)")
        );

        snap.subframes = 2;
        let text = snap.to_text();
        assert!(text.contains("2 iframe(s) not shown"));
        assert!(text.contains("webpilot frame url"));
    }

    #[test]
    fn to_text_renders_captured_page_text() {
        let mut snap = DomSnapshot {
            elements: Vec::new(),
            total_nodes: 0,
            page_url: "x".into(),
            page_title: "y".into(),
            scroll: ScrollInfo::default(),
            scroll_percent: 0,
            extraction_ms: 0,
            subframes: 0,
            shadow_truncated: false,
            text_content: None,
            accessibility_tree: None,
        };
        // Absent until `--include text` populates it.
        assert!(!snap.to_text().contains("Page text"));
        // Present: the captured text must reach the terminal / MCP text path,
        // not only `--json` — it routes through `to_text` like everything else.
        snap.text_content = Some("Hello from the page".into());
        let text = snap.to_text();
        assert!(text.contains("--- Page text ---"));
        assert!(text.contains("Hello from the page"));
    }
}
