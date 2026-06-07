//! Wait condition — a single wait operation per call. Selector / text /
//! navigation / idle are mutually exclusive by construction.

use clap::Subcommand;
use serde::{Deserialize, Serialize};

#[derive(Subcommand, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "until", rename_all = "snake_case")]
pub enum WaitCondition {
    /// Wait until a CSS selector matches.
    Selector { value: String },
    /// Wait until visible page text contains a substring.
    Text {
        // Arbitrary page text can start with `-` (e.g. `wait text -50%`); take
        // it as the value, not a flag — the same free-text rule the action and
        // query args follow. (A CSS selector cannot start with `-`, so
        // `Selector` stays a constrained value, like `find --role`/`--tag`.)
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Wait for the next page navigation (load event).
    Navigation,
    /// Wait until DOM mutations settle for 500ms.
    Idle,
}
