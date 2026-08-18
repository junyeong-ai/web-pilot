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

    /// Total recording duration in seconds (alternative to --frames; fractional allowed).
    #[arg(long)]
    pub duration: Option<f64>,

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
    if args.interval == 0 {
        return Err(webpilot::WebPilotError::InvalidArgument {
            detail: "--interval must be greater than 0".into(),
        }
        .into());
    }
    if args.frames == Some(0) {
        return Err(webpilot::WebPilotError::InvalidArgument {
            detail: "--frames must be greater than 0".into(),
        }
        .into());
    }
    if let Some(secs) = args.duration
        && (!secs.is_finite() || secs <= 0.0)
    {
        return Err(webpilot::WebPilotError::InvalidArgument {
            detail: format!("--duration must be a positive number of seconds (got {secs})"),
        }
        .into());
    }

    let frame_count = match (args.frames, args.duration) {
        // The two are documented as alternatives — each names the same quantity
        // (a frame count) a different way, so supplying both is a contradictory
        // request. Reject it rather than silently honor one and drop the other.
        (Some(_), Some(_)) => {
            return Err(webpilot::WebPilotError::InvalidArgument {
                detail: "specify --frames OR --duration, not both".into(),
            }
            .into());
        }
        (Some(f), None) => f,
        (None, Some(secs)) => {
            let interval_secs = args.interval as f64 / 1000.0;
            ((secs / interval_secs).ceil() as u32).max(1)
        }
        (None, None) => {
            return Err(webpilot::WebPilotError::InvalidArgument {
                detail: "specify --frames or --duration".into(),
            }
            .into());
        }
    };

    // Bound the run: the loop screenshots once per frame, so an unbounded
    // count (a fat-fingered `--frames`, or a huge `--duration`/`--interval`
    // ratio) would otherwise capture for hours and fill the disk.
    let max_frames = webpilot::settings::get().capture.max_record_frames;
    if frame_count > max_frames {
        return Err(webpilot::WebPilotError::InvalidArgument {
            detail: format!(
                "recording of {frame_count} frames exceeds the {max_frames}-frame limit \
                 (raise [capture] max_record_frames to override)"
            ),
        }
        .into());
    }

    // Navigate only after the request is known-valid: the `--frames`/`--duration`
    // contradiction and the frame-count cap are pure checks, so a rejected
    // recording must not first mutate browser state by loading `--url`.
    let mut downloads = Vec::new();
    if let Some(url) = args.url {
        downloads = crate::transport::navigate_to(local, url).await?;
    }

    let dir = dirs::artifacts_dir();
    // pid + nanosecond stamp so two recordings — even concurrent ones in
    // different processes or `--context`s — can't collide on
    // `frame_<stamp>_000.png` and overwrite each other (a `SystemTime` stamp's
    // resolution is coarser than a nanosecond; the pid makes it cross-process
    // unique, matching `dirs::artifact_path`).
    let ts = format!(
        "{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );

    let mut interval =
        tokio::time::interval(std::time::Duration::from_millis(args.interval as u64));

    // Cap the pre-allocation: `frame_count` is user-controlled (up to
    // `u32::MAX` via `--frames`), and `with_capacity` on that raw value would
    // try to reserve tens of GB and abort. The Vec still grows past the hint
    // if a genuinely long recording runs.
    let mut frames: Vec<String> = Vec::with_capacity((frame_count as usize).min(1024));
    let mut dom_files: Vec<String> = Vec::new();

    for i in 0..frame_count {
        interval.tick().await;

        // Capture EVERYTHING this frame needs BEFORE writing any file, so a DOM
        // capture that fails (a `--dom` hard error) can't leave an orphaned
        // screenshot with no matching `.dom.json`. The frame is committed
        // all-or-nothing.
        let b64 = local.page().screenshot().await?;
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64)?;

        let dom_json = if args.dom {
            use webpilot::capture::{CaptureField, CaptureOpts};
            use webpilot::protocol::{Command, ResponseData};
            let r = local
                .send(Command::Capture {
                    include: vec![CaptureField::Dom],
                    opts: CaptureOpts::default(),
                    url: None,
                })
                .await?;
            // `--dom` promises a DOM snapshot per frame, so a frame that didn't
            // produce one is a hard failure, not a silently shorter `dom_files`
            // list reported as success.
            match r {
                ResponseData::Capture {
                    dom: Some(snapshot),
                    ..
                } => Some(serde_json::to_string(&snapshot)?),
                _ => {
                    return Err(webpilot::WebPilotError::Other {
                        detail: format!("record: frame {i} produced no DOM snapshot"),
                    }
                    .into());
                }
            }
        } else {
            None
        };

        // Both captures succeeded — commit the frame's files together.
        let path = dir.join(format!("frame_{ts}_{i:03}.png"));
        std::fs::write(&path, &bytes)?;
        frames.push(path.to_string_lossy().into_owned());
        if let Some(dom) = dom_json {
            let dom_path = dir.join(format!("frame_{ts}_{i:03}.dom.json"));
            std::fs::write(&dom_path, dom)?;
            dom_files.push(dom_path.to_string_lossy().into_owned());
        }

        eprint!("\rFrame {}/{}", i + 1, frame_count);
    }
    eprintln!();

    let mut payload = serde_json::json!({
        "dir": dir.to_string_lossy(),
        "count": frame_count,
        "frames": frames,
    });
    if !downloads.is_empty() {
        payload["downloads"] = serde_json::to_value(&downloads).expect("Download serializes");
    }
    if args.dom {
        payload["dom"] = serde_json::Value::from(dom_files);
    }

    Ok(CommandOutput::Data {
        json: payload,
        human: format!("{} frames -> {}", frame_count, dir.display()),
    })
}
