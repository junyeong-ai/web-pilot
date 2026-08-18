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
    let mut downloads = Vec::new();
    if let Some(url) = args.url {
        downloads = crate::transport::navigate_to(local, url).await?;
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
    // One naming authority (`dirs::artifact_path`): the name carries the pid, so
    // two profiles in the same SystemTime tick — even concurrent ones in
    // different processes / contexts — can't collide and overwrite.
    let path = dirs::artifact_path("profile", "cpuprofile");
    std::fs::write(&path, serde_json::to_string(&data)?)?;

    let mut json = serde_json::json!({"path": path.to_string_lossy()});
    let mut human = format!("Profile saved: {}", path.display());
    // `--url` can land on a file rather than a page; the profile is still the
    // command's result, but the file it wrote is not the agent's to discover.
    if !downloads.is_empty() {
        json["downloads"] = serde_json::to_value(&downloads).expect("Download serializes");
        for d in &downloads {
            human.push('\n');
            human.push_str(&d.to_line());
        }
    }
    Ok(CommandOutput::Data { json, human })
}
