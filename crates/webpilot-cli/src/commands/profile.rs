use clap::Args;

#[derive(Args)]
pub struct ProfileArgs {
    /// Profiling duration in seconds
    #[arg(long)]
    pub duration: u64,

    /// Navigate to URL before profiling
    #[arg(long)]
    pub url: Option<String>,
}
