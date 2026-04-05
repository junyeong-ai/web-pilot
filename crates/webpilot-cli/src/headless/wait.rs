use crate::cdp::CdpClient;
use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

use super::call_bridge;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::wait::WaitArgs,
    output_mode: OutputMode,
) -> Result<()> {
    if args.navigation {
        match cdp
            .wait_for_event(
                "Page.loadEventFired",
                std::time::Duration::from_secs(args.timeout.min(30)),
            )
            .await
        {
            Ok(_) => {
                match output_mode {
                    OutputMode::Human => eprintln!("Navigation complete"),
                    OutputMode::Json => println!("{{\"success\":true}}"),
                }
                return Ok(());
            }
            Err(_) => {
                match output_mode {
                    OutputMode::Human => eprintln!("Navigation timeout ({}s)", args.timeout),
                    OutputMode::Json => println!(
                        "{}",
                        serde_json::json!({"success": false, "error": {"message": "Navigation timeout", "code": "Timeout"}})
                    ),
                }
                anyhow::bail!("Navigation timeout");
            }
        }
    }

    let msg = serde_json::json!({
        "type": "wait",
        "selector": args.selector,
        "text": args.text,
        "timeout_ms": args.timeout * 1000,
    });
    let result = call_bridge(cdp, &msg.to_string()).await?;
    let success = result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if success {
        match output_mode {
            OutputMode::Human => eprintln!("OK"),
            OutputMode::Json => println!("{}", serde_json::json!({"success": true})),
        }
    } else {
        let err_msg = result
            .pointer("/error/message")
            .or(result.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("Wait failed");
        match output_mode {
            OutputMode::Human => eprintln!("Wait failed: {err_msg}"),
            OutputMode::Json => println!("{}", result),
        }
        anyhow::bail!("{err_msg}");
    }

    Ok(())
}
