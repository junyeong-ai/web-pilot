use anyhow::Result;
use clap::Args;
use webpilot::Action;
use webpilot::protocol::{Command, ResponseData};

use webpilot::types::line_safe_clip;

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

#[derive(Args)]
pub struct ActionArgs {
    #[command(subcommand)]
    pub action: Action,

    /// Auto-capture DOM after the action.
    #[arg(long, global = true)]
    pub capture: bool,
}

pub async fn run<T: Transport>(transport: &mut T, mut args: ActionArgs) -> Result<CommandOutput> {
    // Resolve an upload path against the CLI's working directory and confirm it
    // is a readable regular file BEFORE it crosses the wire. Otherwise a relative
    // path would be re-interpreted against Chrome's own cwd (browser mode runs a
    // separate Chrome), and a bad path would surface as a raw CDP error instead
    // of a typed InvalidArgument. `canonicalize` proves the path resolves; a
    // directory, a special file, or an unreadable file (mode 000) would still
    // pass it and then fail deep in `DOM.setFileInputFiles` as an untyped `Other`
    // (exit 1) — so confirm a regular, openable file here and fail as a typed
    // `InvalidArgument` (exit 7) naming the path.
    if let Action::Upload { path, .. } = &mut args.action {
        let resolved =
            std::fs::canonicalize(&path).map_err(|e| webpilot::WebPilotError::InvalidArgument {
                detail: format!("upload path not found: {} ({e})", path.display()),
            })?;
        if !std::fs::metadata(&resolved)
            .map_err(|e| webpilot::WebPilotError::InvalidArgument {
                detail: format!("upload path not readable: {} ({e})", resolved.display()),
            })?
            .is_file()
        {
            return Err(webpilot::WebPilotError::InvalidArgument {
                detail: format!("upload path is not a regular file: {}", resolved.display()),
            }
            .into());
        }
        std::fs::File::open(&resolved).map_err(|e| webpilot::WebPilotError::InvalidArgument {
            detail: format!("upload file not readable: {} ({e})", resolved.display()),
        })?;
        *path = resolved;
    }
    let result = transport
        .send(Command::Action {
            action: args.action,
            capture: args.capture,
        })
        .await?;

    match result {
        ResponseData::Action {
            success,
            error,
            dom,
            url_changed,
            new_tab,
            capture_error,
            ..
        } => {
            lift_error(success, error, ())?;

            if let Some(snapshot) = dom {
                let mut extra = serde_json::Map::new();
                if let Some(ref url) = url_changed {
                    extra.insert("url_changed".into(), serde_json::json!(url));
                }
                if let Some(ref tab) = new_tab {
                    extra.insert(
                        "new_tab".into(),
                        serde_json::to_value(tab).unwrap_or(serde_json::Value::Null),
                    );
                }
                return Ok(CommandOutput::Dom { snapshot, extra });
            }

            let mut msg = String::from("OK");
            if let Some(ref url) = url_changed {
                msg.push_str(&format!("\nURL changed: {}", line_safe_clip(url, 200)));
            }
            if let Some(ref tab) = new_tab {
                msg.push_str(&format!(
                    "\nNew tab opened: {} (switched automatically)",
                    line_safe_clip(&tab.url, 200)
                ));
            }
            if let Some(ref ce) = capture_error {
                msg.push_str(&format!(
                    "\nCapture failed (the action itself succeeded): {ce} — re-run `webpilot capture --include dom`"
                ));
            }

            if url_changed.is_some() || new_tab.is_some() || capture_error.is_some() {
                let mut json = serde_json::json!({"success": true});
                if let Some(ref url) = url_changed {
                    json["url_changed"] = serde_json::json!(url);
                }
                if let Some(ref tab) = new_tab {
                    json["new_tab"] = serde_json::to_value(tab).unwrap_or(serde_json::Value::Null);
                }
                if let Some(ref ce) = capture_error {
                    json["capture_error"] = serde_json::json!(ce);
                }
                Ok(CommandOutput::Data { json, human: msg })
            } else {
                Ok(CommandOutput::Ok(msg))
            }
        }
        ResponseData::Error { error } => Err(error.into()),
        _ => anyhow::bail!("Unexpected response shape"),
    }
}
