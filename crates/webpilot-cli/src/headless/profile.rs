use crate::cdp::CdpClient;
use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::profile::ProfileArgs,
    output_mode: OutputMode,
) -> Result<()> {
    if let Some(ref url) = args.url {
        cdp.navigate(url).await?;
    }

    cdp.send("Profiler.enable", None).await?;
    cdp.send("Profiler.start", None).await?;
    eprintln!("Profiling for {} seconds...", args.duration);
    tokio::time::sleep(std::time::Duration::from_secs(args.duration)).await;
    let result = cdp.send("Profiler.stop", None).await?;
    cdp.send("Profiler.disable", None).await?;

    let profile_data = result.get("profile").cloned().unwrap_or_default();
    let output_dir = std::path::Path::new(webpilot::OUTPUT_DIR);
    std::fs::create_dir_all(output_dir)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = output_dir.join(format!("profile_{ts}.cpuprofile"));
    std::fs::write(&path, serde_json::to_string(&profile_data)?)?;

    match output_mode {
        OutputMode::Human => eprintln!("Profile saved: {}", path.display()),
        OutputMode::Json => println!("{}", serde_json::json!({"path": path.to_string_lossy()})),
    }
    Ok(())
}
