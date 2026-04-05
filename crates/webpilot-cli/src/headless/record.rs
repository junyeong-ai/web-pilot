use crate::cdp::CdpClient;
use crate::commands;
use crate::output::CommandOutput;
use anyhow::Result;

use super::invoke_bridge;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::record::RecordArgs,
) -> Result<CommandOutput> {
    if let Some(ref url) = args.url {
        cdp.navigate(url).await?;
    }

    let frame_count = if let Some(f) = args.frames {
        f
    } else if let Some(d) = args.duration {
        ((d as f64 / args.interval as f64).ceil() as u32).max(1)
    } else {
        anyhow::bail!("Specify --frames or --duration");
    };

    let output_dir = std::path::Path::new(webpilot::OUTPUT_DIR);
    std::fs::create_dir_all(output_dir)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let mut interval =
        tokio::time::interval(std::time::Duration::from_millis(args.interval as u64));
    let mut frames = Vec::new();

    for i in 0..frame_count {
        interval.tick().await;
        let frame_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        let b64 = cdp.screenshot().await?;
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64)?;
        let path = output_dir.join(format!("frame_{ts}_{i:03}.png"));
        std::fs::write(&path, &bytes)?;

        let mut frame = serde_json::json!({
            "index": i,
            "screenshot": path.to_string_lossy(),
            "timestamp_ms": frame_ts as u64,
        });

        if args.dom {
            let dom = invoke_bridge(
                cdp,
                &serde_json::json!({"type": "extractDOM", "options": {}}).to_string(),
            )
            .await?;
            frame["dom"] = dom;
        }

        frames.push(frame);

        eprint!("\rFrame {}/{}", i + 1, frame_count);
    }

    eprintln!("\n{} frames -> {}", frame_count, output_dir.display());

    Ok(CommandOutput::Silent)
}
