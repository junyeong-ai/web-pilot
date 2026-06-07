//! Browser action — single source of truth for both CLI parsing and wire
//! protocol. Derives `clap::Subcommand` and `serde::{Serialize,Deserialize}`
//! on the same enum so that the CLI surface and the on-wire shape can never
//! drift apart.

use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

/// Index of an interactive element from the most recent capture.
pub type ElementIndex = u32;

/// Browser action.
#[derive(Subcommand, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Click an element by index.
    Click { index: ElementIndex },

    /// Type text into an element.
    Type {
        index: ElementIndex,
        text: String,
        /// Replace existing value instead of appending.
        #[arg(long)]
        #[serde(default)]
        clear: bool,
    },

    /// Press a key (with optional modifiers).
    #[command(name = "keypress")]
    KeyPress {
        key: String,
        #[command(flatten)]
        #[serde(default)]
        modifiers: Modifiers,
    },

    /// Navigate to a URL.
    Navigate { url: String },

    /// Scroll the page.
    Scroll {
        direction: ScrollDir,
        #[arg(long, default_value_t = default_scroll_amount())]
        #[serde(default = "default_scroll_amount")]
        amount: u32,
    },

    /// Scroll until an element is in view.
    #[command(name = "scroll-to")]
    ScrollTo { index: ElementIndex },

    /// Browser history: back.
    Back,

    /// Browser history: forward.
    Forward,

    /// Reload the page.
    Reload,

    /// Hover over an element.
    Hover { index: ElementIndex },

    /// Focus an element.
    Focus { index: ElementIndex },

    /// Select an option from a `<select>` element.
    Select { index: ElementIndex, value: String },

    /// Upload a file to an `<input type=file>` element.
    Upload {
        index: ElementIndex,
        path: std::path::PathBuf,
    },

    /// Drag one element to another's position.
    Drag {
        source: ElementIndex,
        target: ElementIndex,
        #[arg(long, default_value_t = default_drag_steps())]
        #[serde(default = "default_drag_steps")]
        steps: u32,
    },
}

#[derive(ValueEnum, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScrollDir {
    Up,
    Down,
}

/// Modifier keys for `KeyPress`.
#[derive(Args, Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct Modifiers {
    #[arg(long)]
    #[serde(default)]
    pub ctrl: bool,
    #[arg(long)]
    #[serde(default)]
    pub shift: bool,
    #[arg(long)]
    #[serde(default)]
    pub alt: bool,
    #[arg(long)]
    #[serde(default)]
    pub meta: bool,
}

/// Categorical action kind for policy lookups and audit logging.
///
/// Wire format matches `Action`'s `kind` discriminator exactly: snake_case.
/// `Display` and `FromStr` are derived from the serde representation, so a
/// value round-trips through any path (CLI text → enum → wire JSON → enum →
/// policy lookup) from one definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
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
}

serde_plain::derive_display_from_serialize!(ActionKind);
serde_plain::derive_fromstr_from_deserialize!(ActionKind);

impl Action {
    pub fn kind(&self) -> ActionKind {
        use Action as A;
        match self {
            A::Click { .. } => ActionKind::Click,
            A::Type { .. } => ActionKind::Type,
            A::KeyPress { .. } => ActionKind::KeyPress,
            A::Navigate { .. } => ActionKind::Navigate,
            A::Back => ActionKind::Back,
            A::Forward => ActionKind::Forward,
            A::Reload => ActionKind::Reload,
            A::Scroll { .. } => ActionKind::Scroll,
            A::ScrollTo { .. } => ActionKind::ScrollTo,
            A::Hover { .. } => ActionKind::Hover,
            A::Focus { .. } => ActionKind::Focus,
            A::Select { .. } => ActionKind::Select,
            A::Upload { .. } => ActionKind::Upload,
            A::Drag { .. } => ActionKind::Drag,
        }
    }
}

fn default_scroll_amount() -> u32 {
    600
}

fn default_drag_steps() -> u32 {
    5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_serializes_with_kind_tag() {
        let a = Action::Click { index: 3 };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["kind"], "click");
        assert_eq!(v["index"], 3);
    }

    #[test]
    fn scroll_direction_uses_lowercase() {
        let a = Action::Scroll {
            direction: ScrollDir::Down,
            amount: 600,
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["direction"], "down");
    }

    #[test]
    fn modifiers_default_omitted_via_default_attr() {
        let a = Action::KeyPress {
            key: "Enter".into(),
            modifiers: Modifiers::default(),
        };
        let v = serde_json::to_value(&a).unwrap();
        let mods = &v["modifiers"];
        assert_eq!(mods["ctrl"], false);
        assert_eq!(mods["shift"], false);
    }

    #[test]
    fn action_kind_matches_variant() {
        assert_eq!(Action::Back.kind(), ActionKind::Back);
        assert_eq!(
            Action::Drag {
                source: 1,
                target: 2,
                steps: 5
            }
            .kind(),
            ActionKind::Drag
        );
    }

    #[test]
    fn action_kind_round_trips() {
        for k in [
            ActionKind::Click,
            ActionKind::Type,
            ActionKind::KeyPress,
            ActionKind::Navigate,
            ActionKind::Scroll,
            ActionKind::ScrollTo,
            ActionKind::Drag,
        ] {
            let parsed: ActionKind = k.to_string().parse().unwrap();
            assert_eq!(parsed, k);
        }
    }
}
