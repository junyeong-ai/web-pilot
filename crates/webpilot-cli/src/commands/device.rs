use anyhow::Result;
use clap::{Args, Subcommand};

use crate::output::CommandOutput;
use crate::transport::LocalTransport;

#[derive(Args)]
pub struct DeviceArgs {
    #[command(subcommand)]
    pub command: DeviceCommand,
}

#[derive(Subcommand)]
pub enum DeviceCommand {
    /// Set custom device metrics.
    Set {
        #[arg(long)]
        width: u32,
        #[arg(long)]
        height: u32,
        #[arg(long)]
        mobile: bool,
        #[arg(long, default_value = "1.0")]
        scale: f64,
        #[arg(long)]
        user_agent: Option<String>,
    },
    /// Use a preset device profile.
    Preset { name: String },
    /// Reset to default (remove emulation).
    Reset,
}

pub async fn run(local: &mut LocalTransport, args: DeviceArgs) -> Result<CommandOutput> {
    let cdp = local.page();
    match args.command {
        DeviceCommand::Set {
            width,
            height,
            mobile,
            scale,
            user_agent,
        } => {
            cdp.send(
                "Emulation.setDeviceMetricsOverride",
                Some(serde_json::json!({
                    "width": width,
                    "height": height,
                    "deviceScaleFactor": scale,
                    "mobile": mobile,
                })),
            )
            .await?;
            if let Some(ua) = user_agent {
                cdp.send(
                    "Emulation.setUserAgentOverride",
                    Some(serde_json::json!({"userAgent": ua})),
                )
                .await?;
            }
            Ok(CommandOutput::Data {
                json: serde_json::json!({"success": true, "width": width, "height": height, "mobile": mobile}),
                human: format!("Device: {width}x{height} (mobile={mobile}, scale={scale})"),
            })
        }
        DeviceCommand::Preset { name } => {
            let (w, h, mobile, scale, ua) = preset(&name).ok_or_else(|| {
                webpilot::WebPilotError::InvalidArgument {
                    detail: format!(
                        "unknown preset '{name}'. Available: iphone-15, iphone-15-pro, pixel-8, ipad-pro, galaxy-s24"
                    ),
                }
            })?;
            cdp.send(
                "Emulation.setDeviceMetricsOverride",
                Some(serde_json::json!({
                    "width": w,
                    "height": h,
                    "deviceScaleFactor": scale,
                    "mobile": mobile,
                })),
            )
            .await?;
            cdp.send(
                "Emulation.setUserAgentOverride",
                Some(serde_json::json!({"userAgent": ua})),
            )
            .await?;
            Ok(CommandOutput::Data {
                json: serde_json::json!({"success": true, "preset": name, "width": w, "height": h}),
                human: format!("Device: {name} ({w}x{h})"),
            })
        }
        DeviceCommand::Reset => {
            // Chrome's `clearDeviceMetricsOverride` removes the override flag
            // but does not trigger a layout pass on its own, so the page can
            // stay at the previously emulated dimensions. Snap the viewport
            // back to the launch size first; the subsequent clear leaves the
            // page in a "no override" state with the natural layout in place.
            let (vw, vh) = crate::session::headless_viewport();
            cdp.send(
                "Emulation.setDeviceMetricsOverride",
                Some(serde_json::json!({
                    "width": vw,
                    "height": vh,
                    "deviceScaleFactor": 1.0,
                    "mobile": false,
                })),
            )
            .await?;
            cdp.send("Emulation.clearDeviceMetricsOverride", None)
                .await?;
            cdp.send(
                "Emulation.setUserAgentOverride",
                Some(serde_json::json!({"userAgent": ""})),
            )
            .await?;
            Ok(CommandOutput::Ok("Device emulation cleared".into()))
        }
    }
}

fn preset(name: &str) -> Option<(u32, u32, bool, f64, &'static str)> {
    Some(match name.to_lowercase().as_str() {
        "iphone-15" | "iphone15" => (
            393,
            852,
            true,
            3.0,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1",
        ),
        "iphone-15-pro" | "iphone15pro" => (
            393,
            852,
            true,
            3.0,
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1",
        ),
        "pixel-8" | "pixel8" => (
            412,
            915,
            true,
            2.625,
            "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36",
        ),
        "ipad-pro" | "ipadpro" => (
            1024,
            1366,
            true,
            2.0,
            "Mozilla/5.0 (iPad; CPU OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/604.1",
        ),
        "galaxy-s24" | "galaxys24" => (
            360,
            780,
            true,
            3.0,
            "Mozilla/5.0 (Linux; Android 14; SM-S921B) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36",
        ),
        _ => return None,
    })
}
