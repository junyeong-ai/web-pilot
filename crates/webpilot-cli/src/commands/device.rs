use anyhow::Result;
use clap::{Args, Subcommand};

use crate::output::CommandOutput;
use crate::transport::LocalTransport;
use crate::transport::local::{DeviceState, clear_persisted_device, write_persisted_device};

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
    // `device` reaches CDP directly (not through `LocalTransport::send`, the usual
    // policy sink), so gate it here: every subcommand changes emulation — the user
    // agent especially is a spoofing effect a `default deny` policy must forbid.
    crate::policy::enforce_key(webpilot::types::PolicyKey::Device)?;
    let ctx = local.persisted_context_key().map(str::to_string);
    match args.command {
        DeviceCommand::Set {
            width,
            height,
            mobile,
            scale,
            user_agent,
        } => {
            // Reject what CDP would only fail on opaquely: a typed exit 7
            // beats a generic emulation error.
            if width == 0 || height == 0 || !scale.is_finite() || scale <= 0.0 {
                return Err(webpilot::WebPilotError::InvalidArgument {
                    detail: format!(
                        "viewport must be positive (got {width}x{height}, scale {scale})"
                    ),
                }
                .into());
            }
            // A control character in the UA is never a legitimate user agent, and
            // the string is sent as a request header and surfaced via
            // `navigator.userAgent`; reject it rather than rely on Chrome
            // stripping it downstream.
            if let Some(ua) = &user_agent
                && ua.chars().any(char::is_control)
            {
                return Err(webpilot::WebPilotError::InvalidArgument {
                    detail: "user agent must not contain control characters".into(),
                }
                .into());
            }
            let state = DeviceState {
                width,
                height,
                mobile,
                scale,
                user_agent,
            };
            state.apply(local.page()).await?;
            // Persist so the override (UA especially) survives this process —
            // re-applied by every later `open`, matching the metrics that
            // already persist incidentally.
            write_persisted_device(ctx.as_deref(), &state)?;
            // Both surfaces carry the COMPLETE applied state — `scale` and the
            // user agent included. The JSON used to omit `scale` and the human
            // line used to omit the UA, so neither alone reflected what was
            // actually emulated. `ua_note` reports default vs custom (the full
            // UA string is long and page-controlled; the agent set it, so the
            // confirmation is what it needs, not an echo).
            let ua_note = match &state.user_agent {
                Some(_) => "custom",
                None => "default",
            };
            Ok(CommandOutput::Data {
                json: serde_json::json!({
                    "success": true, "width": width, "height": height,
                    "mobile": mobile, "scale": scale, "user_agent": state.user_agent,
                }),
                human: format!(
                    "Device: {width}x{height} (mobile={mobile}, scale={scale}, user_agent={ua_note})"
                ),
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
            let state = DeviceState {
                width: w,
                height: h,
                mobile,
                scale,
                user_agent: Some(ua.to_string()),
            };
            state.apply(local.page()).await?;
            write_persisted_device(ctx.as_deref(), &state)?;
            Ok(CommandOutput::Data {
                json: serde_json::json!({"success": true, "preset": name, "width": w, "height": h}),
                human: format!("Device: {name} ({w}x{h})"),
            })
        }
        DeviceCommand::Reset => {
            DeviceState::clear(local.page()).await?;
            // Drop the persisted emulation so a later `open` doesn't re-apply it.
            // The live overrides are already cleared above; if the persisted file
            // can't be removed, say so loudly — leaving it would silently re-apply
            // the device on the next session, contradicting the reset.
            clear_persisted_device(ctx.as_deref()).map_err(|e| {
                webpilot::WebPilotError::Other {
                    detail: format!(
                        "live emulation cleared, but removing the persisted device failed (it would re-apply on the next session): {e}"
                    ),
                }
            })?;
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
