use clap::Args;

#[derive(Args)]
pub struct RecordArgs {
    /// Number of frames to capture
    #[arg(long)]
    pub frames: Option<u32>,

    /// Total recording duration in milliseconds (alternative to --frames)
    #[arg(long)]
    pub duration: Option<u64>,

    /// Interval between frames in milliseconds
    #[arg(long, default_value = "500")]
    pub interval: u32,

    /// Include DOM snapshot per frame
    #[arg(long)]
    pub dom: bool,

    /// Navigate to URL before recording
    #[arg(long)]
    pub url: Option<String>,
}
