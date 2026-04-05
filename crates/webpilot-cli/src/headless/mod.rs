//! Headless command execution via CDP.
//! Translates WebPilot protocol commands into CDP operations.
//! Uses bridge.js functions injected via Runtime.evaluate.

mod action;
mod capture;
mod console;
mod context;
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
use crate::output::OutputMode;
use anyhow::Result;

use context::resolve_context_target;

/// Bridge.js source code (injected into page on first use).
const BRIDGE_JS: &str = include_str!("../../../../extension/content/bridge.js");

/// Shared context for headless command execution.
/// Groups the browser-level CDP connection, the page-level CDP connection,
/// and the WebSocket URL needed for cross-origin reconnection.
pub(crate) struct HeadlessContext {
    pub browser: CdpClient,
    pub ws_url: String,
    pub page: CdpClient,
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
            return status::check(output_mode).await;
        }
        commands::Command::Quit => {
            crate::session::quit_session().await?;
            return Ok(());
        }
        _ => {}
    }

    let ws_url = crate::session::ensure_session().await?;
    let browser = CdpClient::connect(&ws_url).await?;

    // Handle context management subcommand (needs browser, not page cdp)
    if let commands::Command::Context(args) = command {
        return context::run(&browser, args, output_mode).await;
    }

    let page = resolve_target(&browser, &ws_url, context.as_deref()).await?;
    let mut ctx = HeadlessContext {
        browser,
        ws_url,
        page,
    };

    match command {
        commands::Command::Capture(args) => capture::run(&mut ctx, args, output_mode).await,
        commands::Command::Action(args) => action::run(&mut ctx, args, output_mode).await,
        commands::Command::Eval(args) => eval::run(&ctx.page, args, output_mode).await,
        commands::Command::Wait(args) => wait::run(&ctx.page, args, output_mode).await,
        commands::Command::Find(args) => find::run(&ctx.page, args, output_mode).await,
        commands::Command::Status | commands::Command::Quit => {
            anyhow::bail!("internal error: should have been handled above")
        }
        commands::Command::Tabs(args) => tabs::run(&ctx.browser, args, output_mode).await,
        commands::Command::Dom(args) => dom::run(&ctx.page, args, output_mode).await,
        commands::Command::Frames(args) => frames::run(&ctx.page, args, output_mode).await,
        commands::Command::Cookies(args) => cookies::run(&ctx.page, args, output_mode).await,
        commands::Command::Fetch(args) => fetch::run(&ctx.page, args, output_mode).await,
        commands::Command::Network(args) => network::run(&ctx.page, args, output_mode).await,
        commands::Command::Console(args) => console::run(&ctx.page, args, output_mode).await,
        commands::Command::Session(args) => session::run(&ctx.page, args, output_mode).await,
        commands::Command::Policy(args) => policy::run(args, output_mode).await,
        commands::Command::Diff(args) => commands::diff::run(args, output_mode).await,
        commands::Command::Device(args) => device::run(&ctx.page, args, output_mode).await,
        commands::Command::Profile(args) => profile::run(&ctx.page, args, output_mode).await,
        commands::Command::Record(args) => record::run(&ctx.page, args, output_mode).await,
        commands::Command::Context(_) => {
            anyhow::bail!("internal error: Context should have been handled above")
        }
        commands::Command::Install(_) => {
            eprintln!("Install is only needed for --browser mode. Headless works without setup.");
            Ok(())
        }
    }
}

pub(crate) async fn resolve_target(
    browser: &CdpClient,
    ws_url: &str,
    context: Option<&str>,
) -> Result<CdpClient> {
    let target_id = if let Some(ctx_name) = context {
        resolve_context_target(browser, ctx_name).await?
    } else {
        // Default behavior: first page target
        let targets = browser.get_targets().await?;
        targets
            .iter()
            .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            .and_then(|t| t.get("targetId").and_then(|v| v.as_str()))
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("No page target found"))?
    };

    connect_to_page(ws_url, &target_id).await
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

/// Navigate to a URL via CDP, handling cross-origin renderer process swaps.
/// Returns a new CdpClient connected to the page after navigation.
///
/// Design (Playwright-inspired):
///   1. Fire Page.navigate on the page connection (non-blocking: don't wait for CDP response)
///   2. Immediately start polling browser-level targets for the destination URL
///   3. Race: either the page loads at the target URL, or we time out
///   4. Reconnect to the (possibly new) page target
///
/// This avoids the fundamental problem where Chrome may never respond to
/// Page.navigate on the page connection during cross-origin process swaps
/// (about:blank → https, or any cross-origin hop), which would cause a 30s
/// CDP timeout before we even begin waiting for the page to load.
pub(crate) async fn navigate_reconnect(
    browser: &CdpClient,
    ws_url: &str,
    cdp: &CdpClient,
    url: &str,
) -> Result<CdpClient> {
    // Record the current URL before navigation to detect when it changes.
    let current_url = browser
        .get_targets()
        .await
        .ok()
        .and_then(|targets| {
            targets
                .iter()
                .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
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
    // We check two conditions:
    //   1. URL starts with "http" (not about:blank or chrome-error)
    //   2. URL is different from what we had before navigate (handles http→http transitions)
    let deadline = std::time::Instant::now() + crate::timeouts::navigation();
    loop {
        tokio::time::sleep(crate::timeouts::poll_interval()).await;

        if let Ok(targets) = browser.get_targets().await
            && let Some(page) = targets
                .iter()
                .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
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
            anyhow::bail!(
                "Navigation timeout: page did not load within {}s",
                crate::timeouts::navigation().as_secs()
            );
        }
    }

    // Reconnect to the (possibly new) page target
    let targets = browser.get_targets().await?;
    let target_id = targets
        .iter()
        .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
        .and_then(|t| t.get("targetId").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("No page target found after navigation"))?;
    let new_cdp = connect_to_page(ws_url, &target_id).await?;

    // Wait for page to be interactive (DOMContentLoaded).
    // After cross-origin navigation + reconnect, the page may still be loading JS.
    // This ensures bridge.js injection will see the actual DOM, not an empty shell.
    let _ = new_cdp
        .wait_for_event(
            "Page.domContentEventFired",
            std::time::Duration::from_secs(5),
        )
        .await;
    tokio::time::sleep(crate::timeouts::post_navigate()).await;

    Ok(new_cdp)
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

/// Call a bridge.js function via CDP.
/// Delegates to handleMessage() defined in bridge.js (shared with Extension mode).
pub(crate) async fn call_bridge(cdp: &CdpClient, msg_json: &str) -> Result<serde_json::Value> {
    ensure_bridge(cdp).await?;
    let js = format!("(async function() {{ return await handleMessage({msg_json}); }})()");
    cdp.evaluate(&js).await
}
