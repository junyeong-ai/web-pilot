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
        crate::transport::navigate_to(local, url).await?;
    }

    let cdp = local.page();
    cdp.send("Profiler.enable", None).await?;
    cdp.send("Profiler.start", None).await?;
    eprintln!("Profiling for {} seconds...", args.duration);
    tokio::time::sleep(std::time::Duration::from_secs(args.duration)).await;
    let result = cdp.send("Profiler.stop", None).await?;
    cdp.send("Profiler.disable", None).await?;

    // A stop with no `profile` field means the capture produced nothing —
    // writing `null` to a `.cpuprofile` would report an unusable file as
    // success, so surface it instead.
    let data = result
        .get("profile")
        .cloned()
        .ok_or_else(|| webpilot::WebPilotError::Other {
            detail: "Profiler.stop returned no profile data".into(),
        })?;
    let dir = dirs::artifacts_dir();
    // Nanoseconds so two `--context` profiles in the same millisecond don't
    // collide on the filename and overwrite.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!("profile_{ts}.cpuprofile"));
    std::fs::write(&path, serde_json::to_string(&data)?)?;

    Ok(CommandOutput::Data {
        json: serde_json::json!({"path": path.to_string_lossy()}),
        human: format!("Profile saved: {}", path.display()),
    })
}
