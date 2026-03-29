use clap::{Args, Subcommand};

#[derive(Args)]
pub struct DeviceArgs {
    #[command(subcommand)]
    pub action: DeviceAction,
}

#[derive(Subcommand)]
pub enum DeviceAction {
    /// Set custom device metrics
    Set {
        /// Viewport width
        #[arg(long)]
        width: u32,
        /// Viewport height
        #[arg(long)]
        height: u32,
        /// Emulate mobile device
        #[arg(long)]
        mobile: bool,
        /// Device scale factor
        #[arg(long, default_value = "1.0")]
        scale: f64,
        /// Custom user agent string
        #[arg(long)]
        user_agent: Option<String>,
    },
    /// Use a preset device profile (iphone-15, pixel-8, ipad-pro, galaxy-s24)
    Preset {
        /// Device name (iphone-15, pixel-8, ipad-pro, galaxy-s24)
        name: String,
    },
    /// Reset to default (remove emulation)
    Reset,
}
