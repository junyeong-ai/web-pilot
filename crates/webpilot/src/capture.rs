//! Capture specification — what to extract from a page.
//!
//! `CaptureField` lists what to include; `CaptureOpts` controls how to extract.
//! Used by both the CLI parser and the on-wire protocol.

use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};

#[derive(ValueEnum, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CaptureField {
    /// Indexed interactive elements with semantics (default).
    Dom,
    /// Visible page text content.
    Text,
    /// PNG screenshot (saved to file).
    Screenshot,
    /// PDF rendering (saved to file).
    Pdf,
    /// Accessibility tree (saved to file).
    Accessibility,
}

#[derive(Args, Serialize, Deserialize, Debug, Clone, Default)]
pub struct CaptureOpts {
    /// Include bounding box coordinates for each element.
    #[arg(long)]
    #[serde(default)]
    pub bounds: bool,

    /// Capture entire scrollable area (screenshot only; mutually exclusive with `annotate`).
    #[arg(long)]
    #[serde(default)]
    pub full_page: bool,

    /// Detect occluded elements (center-point coverage check).
    #[arg(long)]
    #[serde(default)]
    pub occlusion: bool,

    /// Draw numbered labels on interactive elements before screenshot.
    #[arg(long)]
    #[serde(default)]
    pub annotate: bool,
}

impl CaptureOpts {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.annotate && self.full_page {
            return Err("`annotate` and `full_page` cannot be combined; annotations are viewport-only");
        }
        Ok(())
    }
}

/// Convenience: turn a list of fields into a set-like check helper.
pub fn includes(fields: &[CaptureField], target: CaptureField) -> bool {
    fields.contains(&target)
}
