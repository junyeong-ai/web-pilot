use crate::cdp::CdpClient;
use crate::commands;
use crate::output::CommandOutput;
use anyhow::Result;

use super::{invoke_bridge, parse_bridge_response};

pub(crate) async fn run(cdp: &CdpClient, args: commands::wait::WaitArgs) -> Result<CommandOutput> {
    if args.navigation {
        match cdp
            .wait_for_event(
                "Page.loadEventFired",
                std::time::Duration::from_secs(args.timeout.min(30)),
            )
            .await
        {
            Ok(_) => {
                return Ok(CommandOutput::Ok("Navigation complete".into()));
            }
            Err(_) => {
                return Err(webpilot::types::WebPilotError {
                    code: webpilot::types::ErrorCode::Timeout,
                    message: "Navigation timeout".into(),
                }
                .into());
            }
        }
    }

    let msg = serde_json::json!({
        "type": "wait",
        "selector": args.selector,
        "text": args.text,
        "timeout_ms": args.timeout * 1000,
    });
    let raw = invoke_bridge(cdp, &msg.to_string()).await?;
    parse_bridge_response(raw)?;

    Ok(CommandOutput::Ok("OK".into()))
}
