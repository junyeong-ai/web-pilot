//! Headless command execution via CDP.
//! Translates WebPilot protocol commands into CDP operations.
//! Uses bridge.js functions injected via Runtime.evaluate.

mod action;
mod capture;
mod console;
pub(crate) mod context;
mod cookies;
mod device;
mod dom;
mod eval;
mod fetch;
mod find;
mod frames;
mod network;
mod policy;
mod profile;
mod record;
mod session;
mod status;
mod tabs;
mod wait;

use crate::cdp::CdpClient;
use crate::commands;
use crate::output::{self, CommandOutput, OutputMode};
use anyhow::Result;

use context::resolve_context_target;

/// Bridge.js source code (injected into page on first use).
const BRIDGE_JS: &str = include_str!("../../../../extension/content/bridge.js");

/// Shared context for headless command execution.
/// Groups the browser-level CDP connection, the page-level CDP connection,
/// and the WebSocket URL needed for cross-origin reconnection.
/// When `--context` is used, tracks the CDP BrowserContext for isolation.
pub(crate) struct HeadlessContext {
    pub browser: CdpClient,
    pub ws_url: String,
    pub page: CdpClient,
    pub browser_context_id: Option<String>,
    pub target_id: String,
}

/// Execute a command in headless mode via CDP.
pub async fn run(
    command: commands::Command,
    output_mode: OutputMode,
    context: Option<String>,
) -> Result<()> {
    // Status and Quit don't need to launch Chrome
    match &command {
        commands::Command::Status => {
            let result = status::check(context.as_deref()).await?;
            output::render(result, output_mode);
            return Ok(());
        }
        commands::Command::Quit => {
            if let Some(ref ctx_name) = context {
                crate::session::quit_context(ctx_name).await?;
            } else {
                crate::session::quit_session().await?;
            }
            return Ok(());
        }
        _ => {}
    }

    let ws_url = crate::session::ensure_session().await?;
    let browser = CdpClient::connect(&ws_url).await?;

    // Handle context management subcommand (needs browser-level CDP)
    if let commands::Command::Context(args) = command {
        let result = context::run(&browser, args).await?;
        output::render(result, output_mode);
        return Ok(());
    }

    let (page, browser_context_id, target_id) =
        resolve_target(&browser, &ws_url, context.as_deref()).await?;
    let mut ctx = HeadlessContext {
        browser,
        ws_url,
        page,
        browser_context_id,
        target_id,
    };

    let result = match command {
        commands::Command::Capture(args) => capture::run(&mut ctx, args).await,
        commands::Command::Action(args) => action::run(&mut ctx, args).await,
        commands::Command::Eval(args) => eval::run(&ctx.page, args).await,
        commands::Command::Wait(args) => wait::run(&ctx.page, args).await,
        commands::Command::Find(args) => find::run(&ctx.page, args).await,
        commands::Command::Status | commands::Command::Quit => {
            anyhow::bail!("internal error: should have been handled above")
        }
        commands::Command::Tabs(args) => tabs::run(&ctx, args).await,
        commands::Command::Dom(args) => dom::run(&ctx.page, args).await,
        commands::Command::Frames(args) => frames::run(&ctx.page, args).await,
        commands::Command::Cookies(args) => cookies::run(&ctx.page, args).await,
        commands::Command::Fetch(args) => fetch::run(&ctx.page, args).await,
        commands::Command::Network(args) => network::run(&ctx.page, args).await,
        commands::Command::Console(args) => console::run(&ctx.page, args).await,
        commands::Command::Session(args) => session::run(&ctx.page, args).await,
        commands::Command::Policy(args) => policy::run(args).await,
        commands::Command::Diff(args) => commands::diff::run(args).await,
        commands::Command::Device(args) => device::run(&ctx.page, args).await,
        commands::Command::Profile(args) => profile::run(&ctx.page, args).await,
        commands::Command::Record(args) => record::run(&ctx.page, args).await,
        commands::Command::Context(_) => {
            anyhow::bail!("internal error: Context should have been handled above")
        }
        commands::Command::Install(_) => Ok(CommandOutput::Ok(
            "Install is only needed for --browser mode. Headless works without setup.".into(),
        )),
    };

    match result {
        Ok(cmd_output) => {
            output::render(cmd_output, output_mode);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Resolve the page target. Returns (CdpClient, browser_context_id, target_id).
pub(crate) async fn resolve_target(
    browser: &CdpClient,
    ws_url: &str,
    context: Option<&str>,
) -> Result<(CdpClient, Option<String>, String)> {
    if let Some(ctx_name) = context {
        let target_id = resolve_context_target(browser, ctx_name).await?;
        // Read back browser_context_id from context file
        let file_path = context::context_file_path(ctx_name);
        let browser_context_id = std::fs::read_to_string(&file_path)
            .ok()
            .and_then(|data| serde_json::from_str::<context::ContextEntry>(&data).ok())
            .map(|e| e.browser_context_id);
        let cdp = connect_to_page(ws_url, &target_id).await?;
        Ok((cdp, browser_context_id, target_id))
    } else {
        // Default behavior: first page target
        let targets = browser.get_targets().await?;
        let target = targets
            .iter()
            .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            .ok_or_else(|| webpilot::types::WebPilotError {
                code: webpilot::types::ErrorCode::NoPage,
                message: "No page target found".into(),
            })?;
        let target_id = target
            .get("targetId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cdp = connect_to_page(ws_url, &target_id).await?;
        Ok((cdp, None, target_id))
    }
}

/// Connect to a page target by ID and enable Page + Runtime domains.
pub(crate) async fn connect_to_page(ws_url: &str, target_id: &str) -> Result<CdpClient> {
    let authority = ws_url.split("/devtools/").next().unwrap_or(ws_url);
    let page_ws_url = format!("{authority}/devtools/page/{target_id}");
    let cdp = CdpClient::connect(&page_ws_url).await?;
    cdp.send("Page.enable", None).await?;
    cdp.send("Runtime.enable", None).await?;
    Ok(cdp)
}

/// Find a page target filtered by browserContextId (when in a context) or by targetId.
fn find_page_target<'a>(
    targets: &'a [serde_json::Value],
    browser_context_id: Option<&str>,
    original_target_id: &str,
) -> Option<&'a serde_json::Value> {
    targets
        .iter()
        .find(|t| {
            let is_page = t.get("type").and_then(|v| v.as_str()) == Some("page");
            if !is_page {
                return false;
            }
            if let Some(ctx_id) = browser_context_id {
                t.get("browserContextId").and_then(|v| v.as_str()) == Some(ctx_id)
            } else {
                // No context: match by targetId, fall back to first page
                t.get("targetId").and_then(|v| v.as_str()) == Some(original_target_id)
            }
        })
        .or_else(|| {
            // Fallback: if no exact match, find any page in the context (or first page)
            if browser_context_id.is_some() {
                targets.iter().find(|t| {
                    t.get("type").and_then(|v| v.as_str()) == Some("page")
                        && t.get("browserContextId").and_then(|v| v.as_str()) == browser_context_id
                })
            } else {
                targets
                    .iter()
                    .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            }
        })
}

/// Navigate to a URL via CDP, handling cross-origin renderer process swaps.
/// Returns a new CdpClient and the new target_id.
///
/// Design (Playwright-inspired):
///   1. Fire Page.navigate on the page connection (non-blocking: don't wait for CDP response)
///   2. Immediately start polling browser-level targets for the destination URL
///   3. Race: either the page loads at the target URL, or we time out
///   4. Reconnect to the (possibly new) page target
///
/// When operating in a context, filters targets by browserContextId to avoid
/// reconnecting to another context's page.
pub(crate) async fn navigate_reconnect(
    browser: &CdpClient,
    ws_url: &str,
    cdp: &CdpClient,
    url: &str,
    original_target_id: &str,
    browser_context_id: Option<&str>,
) -> Result<(CdpClient, String)> {
    // Record the current URL before navigation to detect when it changes.
    let current_url = browser
        .get_targets()
        .await
        .ok()
        .and_then(|targets| {
            find_page_target(&targets, browser_context_id, original_target_id)
                .and_then(|t| t.get("url").and_then(|v| v.as_str()))
                .map(|s| s.to_string())
        })
        .unwrap_or_default();

    // Send Page.navigate with a short timeout (3s).
    // Cross-origin navigation (especially about:blank → https) causes Chrome to
    // swap renderer processes, which may kill the page WebSocket connection before
    // Chrome sends a response. A short timeout avoids the default 30s wait.
    let _ = cdp
        .send_with_timeout(
            "Page.navigate",
            Some(serde_json::json!({"url": url})),
            std::time::Duration::from_secs(3),
        )
        .await;

    // Poll browser-level targets until the page URL changes from the original.
    let deadline = std::time::Instant::now() + crate::timeouts::navigation();
    loop {
        tokio::time::sleep(crate::timeouts::poll_interval()).await;

        if let Ok(targets) = browser.get_targets().await
            && let Some(page) = find_page_target(&targets, browser_context_id, original_target_id)
        {
            let page_url = page.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let url_changed = page_url != current_url && page_url != "about:blank";
            let url_is_http = page_url.starts_with("http");
            if url_changed && url_is_http {
                tokio::time::sleep(crate::timeouts::post_reconnect()).await;
                break;
            }
        }

        if std::time::Instant::now() > deadline {
            return Err(webpilot::types::WebPilotError {
                code: webpilot::types::ErrorCode::Timeout,
                message: format!(
                    "Navigation timeout: page did not load within {}s",
                    crate::timeouts::navigation().as_secs()
                ),
            }
            .into());
        }
    }

    // Reconnect to the (possibly new) page target
    let targets = browser.get_targets().await?;
    let target =
        find_page_target(&targets, browser_context_id, original_target_id).ok_or_else(|| {
            webpilot::types::WebPilotError {
                code: webpilot::types::ErrorCode::NoPage,
                message: "No page target found after navigation".into(),
            }
        })?;
    let new_target_id = target
        .get("targetId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let new_cdp = connect_to_page(ws_url, &new_target_id).await?;

    // Wait for page to be interactive (DOMContentLoaded).
    let _ = new_cdp
        .wait_for_event(
            "Page.domContentEventFired",
            std::time::Duration::from_secs(5),
        )
        .await;
    tokio::time::sleep(crate::timeouts::post_navigate()).await;

    Ok((new_cdp, new_target_id))
}

/// Inject bridge.js into the current page.
/// bridge.js now defines all functions at the global scope (no guard wrapping).
/// The message listener is only registered if chrome.runtime exists (Extension mode).
pub(crate) async fn ensure_bridge(cdp: &CdpClient) -> Result<()> {
    let loaded = cdp.evaluate("typeof extractDOM === 'function'").await?;
    if loaded.as_bool() != Some(true) {
        cdp.send(
            "Runtime.evaluate",
            Some(serde_json::json!({
                "expression": BRIDGE_JS,
                "returnByValue": true,
            })),
        )
        .await?;
    }
    Ok(())
}

/// Invoke a bridge.js function via CDP Runtime.evaluate.
/// Delegates to handleMessage() defined in bridge.js (shared with Extension mode).
pub(crate) async fn invoke_bridge(cdp: &CdpClient, msg_json: &str) -> Result<serde_json::Value> {
    ensure_bridge(cdp).await?;
    let js = format!("(async function() {{ return await handleMessage({msg_json}); }})()");
    cdp.evaluate(&js).await
}

/// Parsed bridge.js response.
pub(crate) struct BridgeResponse {
    pub data: serde_json::Value,
}

/// Parse a bridge.js response uniformly.
/// Checks for `{ success: false, error: { message, code } }` pattern.
pub(crate) fn parse_bridge_response(val: serde_json::Value) -> Result<BridgeResponse> {
    let success = val.get("success").and_then(|v| v.as_bool()).unwrap_or(true);
    if !success {
        let msg = val
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .or_else(|| val.get("error").and_then(|v| v.as_str()))
            .unwrap_or("Unknown bridge error");
        let code_str = val
            .pointer("/error/code")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown");
        let code = webpilot::types::ErrorCode::from_str_lossy(code_str);
        return Err(webpilot::types::WebPilotError {
            code,
            message: msg.to_string(),
        }
        .into());
    }
    Ok(BridgeResponse { data: val })
}
