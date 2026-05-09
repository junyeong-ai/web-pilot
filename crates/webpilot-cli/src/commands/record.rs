use anyhow::Result;
use clap::Args;
use webpilot::dirs;

use crate::output::CommandOutput;
use crate::transport::{LocalTransport, Transport};

#[derive(Args)]
pub struct RecordArgs {
    /// Number of frames to capture.
    #[arg(long)]
    pub frames: Option<u32>,

    /// Total recording duration in milliseconds (alternative to --frames).
    #[arg(long)]
    pub duration: Option<u64>,

    /// Interval between frames in milliseconds.
    #[arg(long, default_value = "500")]
    pub interval: u32,

    /// Include DOM snapshot per frame.
    #[arg(long)]
    pub dom: bool,

    /// Navigate to URL before recording.
    #[arg(long)]
    pub url: Option<String>,
}

pub async fn run(local: &mut LocalTransport, args: RecordArgs) -> Result<CommandOutput> {
    if let Some(url) = args.url {
        use webpilot::Action;
        use webpilot::protocol::Command;
        local
            .send(Command::Action {
                action: Action::Navigate { url },
                capture: false,
            })
            .await?;
    }

    let frame_count = match (args.frames, args.duration) {
        (Some(f), _) => f,
        (None, Some(d)) => ((d as f64 / args.interval as f64).ceil() as u32).max(1),
        _ => {
            return Err(webpilot::WebPilotError::InvalidArgument {
                detail: "specify --frames or --duration".into(),
            }
            .into());
        }
    };

    let dir = dirs::artifacts_dir();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let mut interval =
        tokio::time::interval(std::time::Duration::from_millis(args.interval as u64));

    let mut frames: Vec<String> = Vec::with_capacity(frame_count as usize);
    let mut dom_files: Vec<String> = Vec::new();

    for i in 0..frame_count {
        interval.tick().await;

        let b64 = local.page().screenshot().await?;
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64)?;
        let path = dir.join(format!("frame_{ts}_{i:03}.png"));
        std::fs::write(&path, &bytes)?;
        frames.push(path.to_string_lossy().into_owned());

        if args.dom {
            use webpilot::capture::{CaptureField, CaptureOpts};
            use webpilot::protocol::{Command, ResponseData};
            let r = local
                .send(Command::Capture {
                    include: vec![CaptureField::Dom],
                    opts: CaptureOpts::default(),
                    url: None,
                })
                .await?;
            if let ResponseData::Capture {
                dom: Some(snapshot),
                ..
            } = r
            {
                let dom_path = dir.join(format!("frame_{ts}_{i:03}.dom.json"));
                std::fs::write(&dom_path, serde_json::to_string(&snapshot)?)?;
                dom_files.push(dom_path.to_string_lossy().into_owned());
            }
        }

        eprint!("\rFrame {}/{}", i + 1, frame_count);
    }
    eprintln!();

    let mut payload = serde_json::json!({
        "dir": dir.to_string_lossy(),
        "count": frame_count,
        "frames": frames,
    });
    if args.dom {
        payload["dom"] = serde_json::Value::from(dom_files);
    }

    Ok(CommandOutput::Data {
        json: payload,
        human: format!("{} frames -> {}", frame_count, dir.display()),
    })
}
