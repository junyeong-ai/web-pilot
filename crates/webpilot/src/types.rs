use serde::{Deserialize, Serialize};

/// An interactive element extracted from the page DOM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractiveElement {
    pub index: u32,
    pub tag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readonly: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<SelectOption>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landmark: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_viewport: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_new: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occluded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: String,
    pub text: String,
    #[serde(default)]
    pub selected: bool,
}

/// Frame information for iframe navigation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameInfo {
    pub frame_id: i64,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub parent_frame_id: Option<i64>,
    pub is_main: bool,
}

/// Console log entry captured from the page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleEntry {
    pub level: String,
    pub message: String,
    pub timestamp: u64,
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

/// Action policy entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyEntry {
    pub action_type: String,
    pub verdict: String,
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
    pub same_site: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

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

/// Tab info for tab listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabInfo {
    pub id: u32,
    pub url: String,
    pub title: String,
    pub active: bool,
}

/// Serialize a DomSnapshot to LLM-friendly text format.
pub fn serialize_dom(snapshot: &DomSnapshot) -> String {
    let mut out = String::with_capacity(snapshot.elements.len() * 80);

    for el in &snapshot.elements {
        // Index with new-element marker
        let new_marker = if el.is_new == Some(true) { "*" } else { "" };

        // Tag with optional #id suffix
        let tag_id = if let Some(ref id) = el.id {
            format!("{}#{id}", el.tag)
        } else {
            el.tag.clone()
        };
        out.push_str(&format!("{new_marker}[{}] {tag_id} ", el.index));

        if let Some(ref role) = el.role {
            if role != &el.tag {
                out.push_str(&format!("role={role} "));
            }
        }

        if !el.text.is_empty() {
            out.push_str(&format!("\"{}\" ", el.text));
        } else if let Some(ref name) = el.name {
            out.push_str(&format!("\"{}\" ", name));
        }

        if let Some(ref label) = el.label {
            out.push_str(&format!("label=\"{label}\" "));
        }
        // Only show placeholder if different from text
        if let Some(ref ph) = el.placeholder {
            if ph != &el.text {
                out.push_str(&format!("placeholder=\"{ph}\" "));
            }
        }
        if let Some(ref href) = el.href {
            if href.len() > 50 {
                out.push_str(&format!("href=\"{}...\" ", &href[..50]));
            } else {
                out.push_str(&format!("href=\"{href}\" "));
            }
        }
        if let Some(ref val) = el.value {
            if !val.is_empty() {
                out.push_str(&format!("value=\"{val}\" "));
            }
        }
        if let Some(ref it) = el.input_type {
            out.push_str(&format!("type={it} "));
        }
        if let Some(true) = el.checked {
            out.push_str("[checked] ");
        }
        if el.expanded.as_deref() == Some("true") {
            out.push_str("[expanded] ");
        }
        if let Some(true) = el.required {
            out.push_str("[required] ");
        }
        if let Some(true) = el.readonly {
            out.push_str("[readonly] ");
        }
        if el.disabled {
            out.push_str("[disabled] ");
        }
        if el.focused {
            out.push_str("[focused] ");
        }
        if let Some(ref opts) = el.options {
            let sel = opts
                .iter()
                .find(|o| o.selected)
                .map(|o| o.text.as_str())
                .unwrap_or("");
            let count = opts.len();
            out.push_str(&format!("options({count}) selected=\"{sel}\" "));
        }
        if let Some(true) = el.occluded {
            out.push_str("[occluded] ");
        }
        if let Some(false) = el.in_viewport {
            out.push_str("[offscreen] ");
        }
        if let Some(ref frame) = el.frame {
            out.push_str(&format!("frame={frame} "));
        }
        if let Some(ref lm) = el.landmark {
            out.push_str(&format!("@{lm} "));
        }

        out.push('\n');
    }

    // Footer
    out.push_str(&format!(
        "--- Page: {} ({}) ---\n",
        snapshot.page_title, snapshot.page_url
    ));

    let scroll = &snapshot.scroll;
    let above = scroll.pages_above();
    let below = scroll.pages_below();
    let pct = snapshot.scroll_percent;
    if above < 0.05 && below < 0.05 {
        out.push_str("--- Scroll: entire page visible ---\n");
    } else {
        out.push_str(&format!(
            "--- Scroll: {pct}% ({above:.1} above, {below:.1} below) ---\n"
        ));
    }

    out.push_str(&format!(
        "--- {} elements (from {} nodes, {}ms) ---\n",
        snapshot.elements.len(),
        snapshot.total_nodes,
        snapshot.extraction_ms,
    ));

    out
}
