use anyhow::Result;
use clap::Args;
use webpilot::dirs;

use crate::output::CommandOutput;
use crate::transport::LocalTransport;

#[derive(Args)]
pub struct ProfileArgs {
    /// Profiling duration in seconds.
    #[arg(long)]
    pub duration: u64,

    /// Navigate to URL before profiling.
    #[arg(long)]
    pub url: Option<String>,
}

pub async fn run(local: &mut LocalTransport, args: ProfileArgs) -> Result<CommandOutput> {
    if let Some(url) = args.url {
        // Reuse the navigate logic by issuing a Navigate action via Transport.
        use crate::transport::Transport;
        use webpilot::Action;
        use webpilot::protocol::Command;
        local.send(Command::Action {
            action: Action::Navigate { url },
            capture: false,
        })
        .await?;
    }

    let cdp = local.page();
    cdp.send("Profiler.enable", None).await?;
    cdp.send("Profiler.start", None).await?;
    eprintln!("Profiling for {} seconds...", args.duration);
    tokio::time::sleep(std::time::Duration::from_secs(args.duration)).await;
    let result = cdp.send("Profiler.stop", None).await?;
    cdp.send("Profiler.disable", None).await?;

    let data = result.get("profile").cloned().unwrap_or_default();
    let dir = dirs::artifacts_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("profile_{ts}.cpuprofile"));
    std::fs::write(&path, serde_json::to_string(&data)?)?;

    Ok(CommandOutput::Data {
        json: serde_json::json!({"path": path.to_string_lossy()}),
        human: format!("Profile saved: {}", path.display()),
    })
}
