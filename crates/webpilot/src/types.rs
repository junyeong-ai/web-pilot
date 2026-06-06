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
}

// ── Policy ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PolicyVerdict {
    Allow,
    Deny,
}

serde_plain::derive_display_from_serialize!(PolicyVerdict);
serde_plain::derive_fromstr_from_deserialize!(PolicyVerdict);

/// A policy-gated operation. Superset of [`ActionKind`] plus `eval` and `fetch` —
/// the two that run caller-supplied script (directly, or via a `frame find`
/// predicate) and so sit behind the same policy surface. Shares the snake_case
/// wire string space of `ActionKind`, so every key is looked up identically in
/// both transport modes.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyEntry {
    pub operation: PolicyKey,
    pub verdict: PolicyVerdict,
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessibility_tree: Option<String>,
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
            let role_lower = role.to_lowercase();
            let explicit = self
                .role
                .as_ref()
                .is_some_and(|r| r.to_lowercase() == role_lower);
            let implicit = self.implicit_role().is_some_and(|r| r == role_lower);
            let tag_match = self.tag.to_lowercase() == role_lower;
            if !explicit && !implicit && !tag_match {
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

impl DomSnapshot {
    /// Serialize to LLM-friendly text format.
    pub fn to_text(&self) -> String {
        let mut out = String::with_capacity(self.elements.len() * 80);

        for el in &self.elements {
            let new_marker = if el.spatial.is_new == Some(true) {
                "*"
            } else {
                ""
            };

            let tag_id = match &el.id {
                Some(id) => format!("{}#{id}", el.tag),
                None => el.tag.clone(),
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
                out.push_str(&format!("\"{name}\" "));
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
                let trimmed: String = href.chars().take(50).collect();
                if href.chars().count() > 50 {
                    out.push_str(&format!("href=\"{trimmed}...\" "));
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
                out.push_str(&format!("options({}) selected=\"{sel}\" ", opts.len()));
            }
            if el.spatial.occluded == Some(true) {
                out.push_str("[occluded] ");
            }
            if el.spatial.in_viewport == Some(false) {
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

        out.push_str(&format!(
            "--- Page: {} ({}) ---\n",
            self.page_title, self.page_url
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
            text_content: None,
            accessibility_tree: None,
        };
        let _ = snap.to_text(); // Must not panic
    }
}
