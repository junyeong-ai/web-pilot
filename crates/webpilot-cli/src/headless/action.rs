use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

use super::{HeadlessContext, call_bridge, navigate_reconnect};

pub(crate) async fn run(
    ctx: &mut HeadlessContext,
    args: commands::action::ActionArgs,
    output_mode: OutputMode,
) -> Result<()> {
    let browser_action = args.command.to_browser_action()?;
    let action_json = serde_json::to_value(&browser_action)?;
    let cdp = &ctx.page;

    // Handle navigation actions directly via CDP
    match &browser_action {
        webpilot::protocol::BrowserAction::Navigate { url } => {
            let new_cdp = navigate_reconnect(&ctx.browser, &ctx.ws_url, cdp, url).await?;
            ctx.page = new_cdp;
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
            return Ok(());
        }
        webpilot::protocol::BrowserAction::Back => {
            cdp.evaluate("history.back()").await?;
            cdp.wait_for_event("Page.frameNavigated", crate::timeouts::back_forward())
                .await
                .ok();
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
            return Ok(());
        }
        webpilot::protocol::BrowserAction::Forward => {
            cdp.evaluate("history.forward()").await?;
            cdp.wait_for_event("Page.frameNavigated", crate::timeouts::back_forward())
                .await
                .ok();
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
            return Ok(());
        }
        webpilot::protocol::BrowserAction::Reload => {
            cdp.send("Page.reload", None).await?;
            cdp.wait_for_event("Page.loadEventFired", crate::timeouts::reload_wait())
                .await
                .ok();
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
            return Ok(());
        }
        webpilot::protocol::BrowserAction::Drag {
            source,
            target,
            steps,
        } => {
            let coords = call_bridge(
                cdp,
                &serde_json::json!({"type": "getElementCoords", "source": source, "target": target})
                    .to_string(),
            )
            .await?;
            if let Some(err) = coords.get("error").and_then(|v| v.as_str()) {
                anyhow::bail!("{err}");
            }
            let sx = coords["sx"].as_f64().unwrap_or(0.0);
            let sy = coords["sy"].as_f64().unwrap_or(0.0);
            let tx = coords["tx"].as_f64().unwrap_or(0.0);
            let ty = coords["ty"].as_f64().unwrap_or(0.0);

            cdp.send(
                "Input.dispatchMouseEvent",
                Some(serde_json::json!({
                    "type": "mousePressed", "x": sx, "y": sy, "button": "left", "clickCount": 1
                })),
            )
            .await?;
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            for i in 1..=*steps {
                let ratio = i as f64 / *steps as f64;
                cdp.send(
                    "Input.dispatchMouseEvent",
                    Some(serde_json::json!({
                        "type": "mouseMoved",
                        "x": sx + (tx - sx) * ratio,
                        "y": sy + (ty - sy) * ratio,
                        "button": "left"
                    })),
                )
                .await?;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }

            cdp.send(
                "Input.dispatchMouseEvent",
                Some(serde_json::json!({
                    "type": "mouseReleased", "x": tx, "y": ty, "button": "left", "clickCount": 1
                })),
            )
            .await?;

            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
            return Ok(());
        }
        _ => {}
    }

    let result = call_bridge(
        cdp,
        &serde_json::json!({"type": "executeAction", "action": action_json}).to_string(),
    )
    .await?;
    let success = result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    match output_mode {
        OutputMode::Human => {
            if success {
                eprintln!("OK");
            } else {
                let err = result
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                eprintln!("{}", crate::output::format_error_str(err));
            }
        }
        OutputMode::Json => println!("{}", result),
    }
    if !success {
        anyhow::bail!(
            "{}",
            crate::output::format_error_str(
                result
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown"),
            )
        );
    }
    Ok(())
}
