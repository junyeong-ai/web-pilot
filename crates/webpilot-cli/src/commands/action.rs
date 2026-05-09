use anyhow::Result;
use clap::Args;
use webpilot::Action;
use webpilot::protocol::{Command, ResponseData};

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

pub async fn run<T: Transport>(transport: &mut T, args: ActionArgs) -> Result<CommandOutput> {
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
                msg.push_str(&format!("\nURL changed: {url}"));
            }
            if let Some(ref tab) = new_tab {
                msg.push_str(&format!(
                    "\nNew tab opened: {} (switched automatically)",
                    tab.url
                ));
            }

            if url_changed.is_some() || new_tab.is_some() {
                let mut json = serde_json::json!({"success": true});
                if let Some(ref url) = url_changed {
                    json["url_changed"] = serde_json::json!(url);
                }
                if let Some(ref tab) = new_tab {
                    json["new_tab"] = serde_json::to_value(tab).unwrap_or(serde_json::Value::Null);
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
