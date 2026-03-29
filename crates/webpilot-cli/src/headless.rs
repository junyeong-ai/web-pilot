//! Headless command execution via CDP.
//! Translates WebPilot protocol commands into CDP operations.
//! Uses bridge.js functions injected via Runtime.evaluate.

use crate::cdp::CdpClient;
use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

/// Bridge.js source code (injected into page on first use).
const BRIDGE_JS: &str = include_str!("../../../extension/content/bridge.js");

/// Execute a command in headless mode via CDP.
pub async fn run(command: commands::Command, output_mode: OutputMode) -> Result<()> {
    // Status and Quit don't need to launch Chrome
    match &command {
        commands::Command::Status => {
            return status_check(output_mode).await;
        }
        commands::Command::Quit => {
            crate::session::quit_session()?;
            return Ok(());
        }
        _ => {}
    }

    let ws_url = crate::session::ensure_session()?;
    let browser = CdpClient::connect(&ws_url).await?;

    // Get the first page target's WebSocket URL
    let targets = browser.get_targets().await?;
    let page_target = targets
        .iter()
        .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
        .ok_or_else(|| anyhow::anyhow!("No page target found"))?;

    let target_id = page_target
        .get("targetId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("No targetId"))?;

    // Construct page-level WebSocket URL: ws://host:port/devtools/page/TARGET_ID
    let authority = ws_url.split("/devtools/").next().unwrap_or(&ws_url);
    let page_ws_url = format!("{authority}/devtools/page/{target_id}");
    let cdp = CdpClient::connect(&page_ws_url).await?;

    // Enable required CDP domains on page target
    cdp.send("Page.enable", None).await?;
    cdp.send("Runtime.enable", None).await?;

    match command {
        commands::Command::Capture(args) => capture(&cdp, args, output_mode).await,
        commands::Command::Action(args) => action(&cdp, args, output_mode).await,
        commands::Command::Eval(args) => eval(&cdp, args, output_mode).await,
        commands::Command::Wait(args) => wait(&cdp, args, output_mode).await,
        commands::Command::Find(args) => find(&cdp, args, output_mode).await,
        commands::Command::Status | commands::Command::Quit => {
            anyhow::bail!("internal error: should have been handled above")
        }
        commands::Command::Tabs(args) => tabs(&cdp, args, output_mode).await,
        commands::Command::Dom(args) => dom(&cdp, args, output_mode).await,
        commands::Command::Frames(_) => frames(&cdp, output_mode).await,
        commands::Command::Cookies(args) => cookies(&cdp, args, output_mode).await,
        commands::Command::Fetch(args) => fetch(&cdp, args, output_mode).await,
        commands::Command::Network(args) => network(&cdp, args, output_mode).await,
        commands::Command::Console(args) => console(&cdp, args, output_mode).await,
        commands::Command::Session(args) => session(&cdp, args, output_mode).await,
        commands::Command::Policy(args) => policy(args, output_mode).await,
        commands::Command::Diff(args) => commands::diff::run(args, output_mode).await,
        commands::Command::Device(args) => device(&cdp, args, output_mode).await,
        commands::Command::Profile(args) => profile(&cdp, args, output_mode).await,
        commands::Command::Record(args) => record(&cdp, args, output_mode).await,
        commands::Command::Install(_) => {
            eprintln!("Install is only needed for --browser mode. Headless works without setup.");
            Ok(())
        }
    }
}

async fn status_check(output_mode: OutputMode) -> Result<()> {
    let Some(ws_url) = crate::session::get_existing_session() else {
        match output_mode {
            OutputMode::Human => eprintln!("Mode: headless\nStatus: no active session"),
            OutputMode::Json => println!(
                "{}",
                serde_json::json!({"connected": false, "mode": "headless"})
            ),
        }
        return Ok(());
    };

    let Ok(browser) = CdpClient::connect(&ws_url).await else {
        match output_mode {
            OutputMode::Human => eprintln!("Mode: headless\nStatus: disconnected"),
            OutputMode::Json => println!(
                "{}",
                serde_json::json!({"connected": false, "mode": "headless"})
            ),
        }
        return Ok(());
    };

    let targets = browser.get_targets().await?;
    let page_target = targets
        .iter()
        .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"));

    if let Some(pt) = page_target {
        let target_id = pt.get("targetId").and_then(|v| v.as_str()).unwrap_or("");
        let authority = ws_url.split("/devtools/").next().unwrap_or(&ws_url);
        let page_ws_url = format!("{authority}/devtools/page/{target_id}");
        let cdp = CdpClient::connect(&page_ws_url).await?;
        cdp.send("Runtime.enable", None).await?;
        status(&cdp, output_mode).await
    } else {
        match output_mode {
            OutputMode::Human => eprintln!("Mode: headless\nStatus: connected (no page tab)"),
            OutputMode::Json => println!(
                "{}",
                serde_json::json!({"connected": true, "mode": "headless"})
            ),
        }
        Ok(())
    }
}

/// Inject bridge.js into the current page.
/// bridge.js now defines all functions at the global scope (no guard wrapping).
/// The message listener is only registered if chrome.runtime exists (Extension mode).
async fn ensure_bridge(cdp: &CdpClient) -> Result<()> {
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
async fn call_bridge(cdp: &CdpClient, msg_json: &str) -> Result<serde_json::Value> {
    ensure_bridge(cdp).await?;
    let js = format!("(async function() {{ return await handleMessage({msg_json}); }})()");
    cdp.evaluate(&js).await
}

async fn capture(
    cdp: &CdpClient,
    args: commands::capture::CaptureArgs,
    output_mode: OutputMode,
) -> Result<()> {
    // Validate flag conflicts
    if args.annotate && args.full_page {
        anyhow::bail!(
            "--annotate and --full-page cannot be combined. Annotations are viewport-only."
        );
    }

    // Navigate if URL provided
    if let Some(ref url) = args.url {
        cdp.navigate(url).await?;
    }

    let mut out = serde_json::Map::new();

    // DOM extraction
    let dom = args.dom || args.annotate || (!args.screenshot && !args.text && !args.accessibility);
    let bounds = args.bounds || args.annotate;

    if dom {
        let opts = serde_json::json!({"bounds": bounds, "occlusion": args.occlusion});
        let result = call_bridge(
            cdp,
            &serde_json::json!({"type": "extractDOM", "options": opts}).to_string(),
        )
        .await?;
        if let Some(obj) = result.as_object() {
            for (k, v) in obj {
                out.insert(k.clone(), v.clone());
            }
        }
    }

    // Text
    if args.text {
        let result =
            call_bridge(cdp, &serde_json::json!({"type": "extractText"}).to_string()).await?;
        if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
            out.insert("text_content".into(), serde_json::json!(text));
        }
    }

    // Screenshot
    let screenshot = args.screenshot || args.annotate;
    if screenshot {
        // Add annotations if requested
        if args.annotate
            && let Some(elements) = out.get("elements").and_then(|v| v.as_array())
        {
            let annotations: Vec<_> = elements
                .iter()
                .filter(|e| {
                    e.get("in_viewport")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                        && e.get("bounds").is_some()
                })
                .filter_map(|e| {
                    let b = e.get("bounds")?;
                    Some(serde_json::json!({
                        "index": e.get("index")?,
                        "x": b.get("x")?, "y": b.get("y")?,
                        "w": b.get("w")?, "h": b.get("h")?,
                    }))
                })
                .collect();
            if !annotations.is_empty() {
                call_bridge(
                    cdp,
                    &serde_json::json!({"type": "addAnnotations", "elements": annotations})
                        .to_string(),
                )
                .await?;
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }

        let b64 = cdp.screenshot().await?;

        // Remove annotations
        if args.annotate {
            let _ = call_bridge(
                cdp,
                &serde_json::json!({"type": "removeAnnotations"}).to_string(),
            )
            .await;
        }

        // Save screenshot
        let output_dir = std::path::Path::new(webpilot::OUTPUT_DIR);
        let _ = std::fs::create_dir_all(output_dir);
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64)?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let path = output_dir.join(format!("capture_{ts}.png"));
        std::fs::write(&path, &bytes)?;
        out.insert(
            "screenshot_path".into(),
            serde_json::json!(path.to_string_lossy()),
        );
    }

    // PDF generation
    if args.pdf {
        let pdf_result = cdp
            .send(
                "Page.printToPDF",
                Some(serde_json::json!({
                    "landscape": false,
                    "printBackground": true,
                    "preferCSSPageSize": true,
                })),
            )
            .await?;
        let data = pdf_result
            .get("data")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("PDF generation failed: no data returned"))?;
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)?;
        let output_dir = std::path::Path::new(webpilot::OUTPUT_DIR);
        std::fs::create_dir_all(output_dir)?;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let pdf_path = output_dir.join(format!("capture_{ts}.pdf"));
        std::fs::write(&pdf_path, &bytes)?;
        out.insert(
            "pdf_path".into(),
            serde_json::json!(pdf_path.to_string_lossy()),
        );
    }

    // Output
    match output_mode {
        OutputMode::Human => {
            if let Some(elements) = out.get("elements").and_then(|v| v.as_array())
                && !elements.is_empty()
            {
                let snapshot: webpilot::types::DomSnapshot =
                    serde_json::from_value(serde_json::Value::Object(out.clone()))?;
                print!("{}", webpilot::types::serialize_dom(&snapshot));
            }
            if let Some(path) = out.get("screenshot_path").and_then(|v| v.as_str()) {
                eprintln!("Screenshot: {path}");
            }
            if let Some(path) = out.get("pdf_path").and_then(|v| v.as_str()) {
                eprintln!("PDF: {path}");
            }
        }
        OutputMode::Json => {
            println!("{}", serde_json::Value::Object(out));
        }
    }
    Ok(())
}

async fn action(
    cdp: &CdpClient,
    args: commands::action::ActionArgs,
    output_mode: OutputMode,
) -> Result<()> {
    let browser_action = args.action.to_browser_action()?;
    let action_json = serde_json::to_value(&browser_action)?;

    // Handle navigation actions directly via CDP
    match &browser_action {
        webpilot::protocol::BrowserAction::Navigate { url } => {
            cdp.navigate(url).await?;
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
            return Ok(());
        }
        webpilot::protocol::BrowserAction::Back => {
            cdp.evaluate("history.back()").await?;
            cdp.wait_for_event("Page.frameNavigated", std::time::Duration::from_secs(5))
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
            cdp.wait_for_event("Page.frameNavigated", std::time::Duration::from_secs(5))
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
            cdp.wait_for_event("Page.loadEventFired", std::time::Duration::from_secs(10))
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
                let code = result.get("code").and_then(|v| v.as_str());
                eprintln!("{}", crate::output::format_error(err, code));
            }
        }
        OutputMode::Json => println!("{}", result),
    }
    if !success {
        anyhow::bail!(
            "{}",
            crate::output::format_error(
                result
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown"),
                result.get("code").and_then(|v| v.as_str()),
            )
        );
    }
    Ok(())
}

async fn eval(
    cdp: &CdpClient,
    args: commands::eval::EvalArgs,
    output_mode: OutputMode,
) -> Result<()> {
    let result = cdp.evaluate(&format!("({})", args.code)).await;
    match result {
        Ok(val) => {
            let json_str = serde_json::to_string(&val)?;
            match output_mode {
                OutputMode::Human => println!("{json_str}"),
                OutputMode::Json => println!(
                    "{}",
                    serde_json::json!({"success": true, "result": json_str})
                ),
            }
        }
        Err(e) => {
            match output_mode {
                OutputMode::Human => {
                    eprintln!("{}", crate::output::format_error(&e.to_string(), None))
                }
                OutputMode::Json => println!(
                    "{}",
                    serde_json::json!({"success": false, "error": e.to_string()})
                ),
            }
            anyhow::bail!("{}", crate::output::format_error(&e.to_string(), None));
        }
    }
    Ok(())
}

async fn wait(
    cdp: &CdpClient,
    args: commands::wait::WaitArgs,
    _output_mode: OutputMode,
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
                println!("{{\"success\":true}}");
                return Ok(());
            }
            Err(_) => {
                println!(
                    "{{\"success\":false,\"error\":\"Navigation timeout\",\"code\":\"TIMEOUT\"}}"
                );
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
    println!("{}", result);
    if !success {
        anyhow::bail!(
            "{}",
            crate::output::format_error(
                result
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Wait failed"),
                result.get("code").and_then(|v| v.as_str()),
            )
        );
    }
    Ok(())
}

async fn find(
    cdp: &CdpClient,
    args: commands::find::FindArgs,
    output_mode: OutputMode,
) -> Result<()> {
    if args.role.is_none()
        && args.text.is_none()
        && args.label.is_none()
        && args.placeholder.is_none()
        && args.tag.is_none()
    {
        anyhow::bail!(
            "At least one filter required: --role, --text, --label, --placeholder, or --tag"
        );
    }

    let dom_result = call_bridge(
        cdp,
        &serde_json::json!({"type": "extractDOM", "options": {}}).to_string(),
    )
    .await?;
    let elements: Vec<webpilot::types::InteractiveElement> = serde_json::from_value(
        dom_result
            .get("elements")
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![])),
    )?;

    let filter = webpilot::types::ElementFilter {
        role: args.role.clone(),
        text: args.text.clone(),
        label: args.label.clone(),
        placeholder: args.placeholder.clone(),
        tag: args.tag.clone(),
    };

    let matches: Vec<&webpilot::types::InteractiveElement> =
        elements.iter().filter(|el| el.matches(&filter)).collect();

    match output_mode {
        OutputMode::Human => {
            for el in &matches {
                eprintln!("[{}] {} \"{}\"", el.index, el.tag, el.text);
            }
            eprintln!("({} matches)", matches.len());
        }
        OutputMode::Json => println!(
            "{}",
            serde_json::json!({"matches": matches, "count": matches.len()})
        ),
    }

    if matches.is_empty() {
        anyhow::bail!("No matching elements found");
    }

    let first_index = matches[0].index;
    if args.click {
        let action_json = serde_json::json!({"action": "Click", "index": first_index});
        call_bridge(
            cdp,
            &serde_json::json!({"type": "executeAction", "action": action_json}).to_string(),
        )
        .await?;
        if output_mode == OutputMode::Human {
            eprintln!("OK");
        }
    } else if let Some(ref fill_text) = args.fill {
        let action_json = serde_json::json!({"action": "Type", "index": first_index, "text": fill_text, "clear": true});
        call_bridge(
            cdp,
            &serde_json::json!({"type": "executeAction", "action": action_json}).to_string(),
        )
        .await?;
        if output_mode == OutputMode::Human {
            eprintln!("OK");
        }
    }
    Ok(())
}

async fn status(cdp: &CdpClient, output_mode: OutputMode) -> Result<()> {
    let version = cdp.send("Browser.getVersion", None).await?;
    let title = cdp
        .evaluate("document.title")
        .await
        .unwrap_or(serde_json::Value::Null);
    let url = cdp
        .evaluate("location.href")
        .await
        .unwrap_or(serde_json::Value::Null);

    match output_mode {
        OutputMode::Human => {
            eprintln!("Mode: headless");
            eprintln!(
                "Chrome: {}",
                version
                    .get("product")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
            );
            if let Some(t) = title.as_str() {
                eprintln!("Tab: {t}");
            }
            if let Some(u) = url.as_str() {
                eprintln!("URL: {u}");
            }
        }
        OutputMode::Json => {
            println!(
                "{}",
                serde_json::json!({
                    "connected": true,
                    "mode": "headless",
                    "chrome_version": version.get("product"),
                    "tab_title": title,
                    "tab_url": url,
                })
            );
        }
    }
    Ok(())
}

async fn dom(cdp: &CdpClient, args: commands::dom::DomArgs, output_mode: OutputMode) -> Result<()> {
    let msg = match &args.action {
        commands::dom::DomAction::SetHtml { selector, value } => {
            serde_json::json!({"type": "setHtml", "selector": selector, "value": value})
        }
        commands::dom::DomAction::SetText { selector, value } => {
            serde_json::json!({"type": "setText", "selector": selector, "value": value})
        }
        commands::dom::DomAction::SetAttr {
            selector,
            attr,
            value,
        } => {
            serde_json::json!({"type": "setAttr", "selector": selector, "attr": attr, "value": value})
        }
        commands::dom::DomAction::GetHtml { selector } => {
            serde_json::json!({"type": "getHtml", "selector": selector})
        }
        commands::dom::DomAction::GetText { selector } => {
            serde_json::json!({"type": "getText", "selector": selector})
        }
        commands::dom::DomAction::GetAttr { selector, attr } => {
            serde_json::json!({"type": "getAttr", "selector": selector, "attr": attr})
        }
    };
    let result = call_bridge(cdp, &msg.to_string()).await?;
    let success = result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if let Some(val) = result.get("value").and_then(|v| v.as_str()) {
        match output_mode {
            OutputMode::Human => println!("{val}"),
            OutputMode::Json => println!("{}", serde_json::json!({"success": true, "value": val})),
        }
    } else if success {
        match output_mode {
            OutputMode::Human => eprintln!("OK"),
            OutputMode::Json => println!("{{\"success\":true}}"),
        }
    }
    if !success {
        let err = result
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        anyhow::bail!("{err}");
    }
    Ok(())
}

async fn frames(cdp: &CdpClient, output_mode: OutputMode) -> Result<()> {
    let result = cdp.send("Page.getFrameTree", None).await?;
    match output_mode {
        OutputMode::Human => eprintln!("Frame tree: {}", serde_json::to_string_pretty(&result)?),
        OutputMode::Json => println!("{}", result),
    }
    Ok(())
}

async fn cookies(
    cdp: &CdpClient,
    args: commands::cookies::CookiesArgs,
    output_mode: OutputMode,
) -> Result<()> {
    match args.action {
        commands::cookies::CookiesAction::List { url } => {
            let result = cdp
                .send(
                    "Network.getCookies",
                    Some(serde_json::json!({"urls": [url]})),
                )
                .await?;
            let cookies = result
                .get("cookies")
                .cloned()
                .unwrap_or(serde_json::Value::Array(vec![]));
            match output_mode {
                OutputMode::Human => {
                    if let Some(arr) = cookies.as_array() {
                        for c in arr {
                            let val = c.get("value").and_then(|v| v.as_str()).unwrap_or("");
                            eprintln!(
                                "{}={} ({})",
                                c.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                                val.get(..20).unwrap_or(val),
                                c.get("domain").and_then(|v| v.as_str()).unwrap_or("")
                            );
                        }
                    }
                }
                OutputMode::Json => println!("{}", cookies),
            }
        }
        commands::cookies::CookiesAction::Get { url, name } => {
            let result = cdp
                .send(
                    "Network.getCookies",
                    Some(serde_json::json!({"urls": [url]})),
                )
                .await?;
            let cookie = result
                .get("cookies")
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    arr.iter()
                        .find(|c| c.get("name").and_then(|v| v.as_str()) == Some(&name))
                })
                .cloned();
            if let Some(ref c) = cookie {
                match output_mode {
                    OutputMode::Human => {
                        println!("{}", c.get("value").and_then(|v| v.as_str()).unwrap_or(""));
                    }
                    OutputMode::Json => println!("{c}"),
                }
            } else {
                anyhow::bail!("Cookie '{name}' not found");
            }
        }
        commands::cookies::CookiesAction::Set {
            url,
            name,
            value,
            httponly,
            secure,
        } => {
            cdp.send(
                "Network.setCookie",
                Some(serde_json::json!({
                    "url": url,
                    "name": name,
                    "value": value,
                    "httpOnly": httponly,
                    "secure": secure,
                })),
            )
            .await?;
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
        }
        commands::cookies::CookiesAction::Delete { url, name } => {
            cdp.send(
                "Network.deleteCookies",
                Some(serde_json::json!({
                    "url": url,
                    "name": name,
                })),
            )
            .await?;
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
        }
    }
    Ok(())
}

async fn fetch(
    cdp: &CdpClient,
    args: commands::fetch::FetchArgs,
    output_mode: OutputMode,
) -> Result<()> {
    let method = &args.method;
    let body_js = if let Some(ref body) = args.body {
        format!("body: {}, ", serde_json::to_string(body)?)
    } else {
        String::new()
    };
    let js = format!(
        r#"fetch({url}, {{method: {method}, credentials: "include", headers: {{"Content-Type": "application/json"}}, {body_js}}}).then(r => r.text().then(body => ({{status: r.status, body}})))"#,
        url = serde_json::to_string(&args.url)?,
        method = serde_json::to_string(method)?,
    );
    let result = cdp.evaluate(&js).await?;
    match output_mode {
        OutputMode::Human => {
            if let Some(body) = result.get("body").and_then(|v| v.as_str()) {
                println!("{body}");
            }
            eprintln!(
                "HTTP {}",
                result.get("status").and_then(|v| v.as_u64()).unwrap_or(0)
            );
        }
        OutputMode::Json => println!(
            "{}",
            serde_json::json!({"success": true, "status": result.get("status"), "body": result.get("body")})
        ),
    }
    Ok(())
}

async fn network(
    cdp: &CdpClient,
    args: commands::network::NetworkArgs,
    output_mode: OutputMode,
) -> Result<()> {
    match args.action {
        commands::network::NetworkAction::Start => {
            call_bridge(
                cdp,
                &serde_json::json!({"type": "executeAction", "action": {"action": "noop"}})
                    .to_string(),
            )
            .await
            .ok(); // ensure bridge loaded
            // Inject network monitoring via the same MAIN-world pattern
            cdp.evaluate(r#"
                if (!window.__webpilot_network_active) {
                    window.__webpilot_network_active = true;
                    window.__webpilot_network = [];
                    const origFetch = window.fetch;
                    window.fetch = function(...args) {
                        const [resource, config] = args;
                        const t0 = performance.now();
                        return origFetch.apply(this, args).then(response => {
                            window.__webpilot_network.push({ type: "fetch", url: String(resource), method: config?.method || "GET", status: response.status, duration_ms: Math.round(performance.now() - t0), timestamp: Date.now() });
                            if (window.__webpilot_network.length > 500) window.__webpilot_network.shift();
                            return response;
                        }).catch(err => { window.__webpilot_network.push({ type: "fetch", url: String(resource), method: config?.method || "GET", error: err.message, duration_ms: Math.round(performance.now() - t0), timestamp: Date.now() }); throw err; });
                    };
                }
                true
            "#).await?;
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
        }
        commands::network::NetworkAction::Read { since } => {
            let js = format!(
                "(window.__webpilot_network || []).filter(e => e.timestamp >= {})",
                since.unwrap_or(0)
            );
            let result = cdp.evaluate(&js).await?;
            match output_mode {
                OutputMode::Human => {
                    if let Some(arr) = result.as_array() {
                        for r in arr {
                            eprintln!(
                                "{} {} {} → {}",
                                r.get("type").and_then(|v| v.as_str()).unwrap_or("?"),
                                r.get("method").and_then(|v| v.as_str()).unwrap_or("?"),
                                r.get("url")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?")
                                    .get(..60)
                                    .unwrap_or(""),
                                r.get("status").and_then(|v| v.as_u64()).unwrap_or(0)
                            );
                        }
                        eprintln!("({} requests)", arr.len());
                    }
                }
                OutputMode::Json => println!("{}", result),
            }
        }
        commands::network::NetworkAction::Clear => {
            cdp.evaluate("window.__webpilot_network = []").await?;
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
        }
    }
    Ok(())
}

async fn console(
    cdp: &CdpClient,
    args: commands::console::ConsoleArgs,
    output_mode: OutputMode,
) -> Result<()> {
    match args.action {
        commands::console::ConsoleAction::Start => {
            cdp.evaluate(r#"
                if (!window.__webpilot_console) {
                    window.__webpilot_console = [];
                    const orig = { log: console.log, error: console.error, warn: console.warn, info: console.info };
                    ["log", "error", "warn", "info"].forEach(m => {
                        console[m] = (...args) => {
                            window.__webpilot_console.push({ level: m, message: args.map(a => { try { return String(a); } catch { return "[object]"; } }).join(" "), timestamp: Date.now() });
                            if (window.__webpilot_console.length > 500) window.__webpilot_console.shift();
                            orig[m].apply(console, args);
                        };
                    });
                }
                true
            "#).await?;
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
        }
        commands::console::ConsoleAction::Read { level } => {
            let result = cdp.evaluate("window.__webpilot_console || []").await?;
            let entries: Vec<serde_json::Value> = if let Some(arr) = result.as_array() {
                if let Some(ref lvl) = level {
                    arr.iter()
                        .filter(|e| e.get("level").and_then(|v| v.as_str()) == Some(lvl.as_str()))
                        .cloned()
                        .collect()
                } else {
                    arr.clone()
                }
            } else {
                vec![]
            };
            match output_mode {
                OutputMode::Human => {
                    for e in &entries {
                        eprintln!(
                            "[{}] {}",
                            e.get("level").and_then(|v| v.as_str()).unwrap_or("?"),
                            e.get("message").and_then(|v| v.as_str()).unwrap_or("")
                        );
                    }
                    eprintln!("({} entries)", entries.len());
                }
                OutputMode::Json => println!("{}", serde_json::json!(entries)),
            }
        }
        commands::console::ConsoleAction::Clear => {
            cdp.evaluate("window.__webpilot_console = []").await?;
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
        }
    }
    Ok(())
}

async fn session(
    cdp: &CdpClient,
    args: commands::session::SessionArgs,
    output_mode: OutputMode,
) -> Result<()> {
    match args.action {
        commands::session::SessionAction::Export { output } => {
            let cookies = cdp.get_cookies().await?;
            let storage = call_bridge(
                cdp,
                &serde_json::json!({"type": "exportStorage"}).to_string(),
            )
            .await
            .unwrap_or(serde_json::json!({"localStorage": {}, "sessionStorage": {}}));
            let data = serde_json::json!({
                "version": 1, "exported_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                "cookies": cookies, "local_storage": storage.get("localStorage"), "session_storage": storage.get("sessionStorage"),
            });
            let dir = std::path::Path::new(webpilot::OUTPUT_DIR);
            let _ = std::fs::create_dir_all(dir);
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let path = if let Some(ref p) = output {
                std::path::PathBuf::from(p)
            } else {
                dir.join(format!("session_{ts}.json"))
            };
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&path, serde_json::to_string_pretty(&data)?)?;
            match output_mode {
                OutputMode::Human => eprintln!("Session exported: {}", path.display()),
                OutputMode::Json => {
                    println!("{}", serde_json::json!({"path": path.to_string_lossy()}))
                }
            }
        }
        commands::session::SessionAction::Import { path } => {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("Cannot read session file: {e}"))?;
            let data: serde_json::Value = serde_json::from_str(&content)?;
            // Import cookies via CDP
            if let Some(cookies) = data.get("cookies").and_then(|v| v.as_array()) {
                for c in cookies {
                    let _ = cdp.send("Network.setCookie", Some(c.clone())).await;
                }
                match output_mode {
                    OutputMode::Human => eprintln!("Imported {} cookies", cookies.len()),
                    OutputMode::Json => println!(
                        "{}",
                        serde_json::json!({"success": true, "cookies_imported": cookies.len()})
                    ),
                }
            }
            // Import localStorage via bridge
            if let Some(ls) = data.get("local_storage") {
                let msg = serde_json::json!({"type": "importStorage", "localStorage": ls});
                call_bridge(cdp, &msg.to_string()).await?;
            }
        }
    }
    Ok(())
}

/// File-based policy store (persists across CLI invocations).
fn policy_file() -> std::path::PathBuf {
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    std::path::Path::new(webpilot::OUTPUT_DIR).join(format!("{user}-policies.json"))
}

fn read_policies() -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(policy_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_policies(policies: &std::collections::HashMap<String, String>) {
    let _ = std::fs::write(
        policy_file(),
        serde_json::to_string(policies).unwrap_or_default(),
    );
}

async fn policy(args: commands::policy::PolicyArgs, output_mode: OutputMode) -> Result<()> {
    match args.action {
        commands::policy::PolicyAction::Set { action, verdict } => {
            let mut policies = read_policies();
            policies.insert(action, verdict);
            write_policies(&policies);
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
        }
        commands::policy::PolicyAction::List => {
            let policies = read_policies();
            let list: Vec<_> = policies
                .iter()
                .map(|(k, v)| serde_json::json!({"action_type": k, "verdict": v}))
                .collect();
            match output_mode {
                OutputMode::Human => {
                    for p in &list {
                        eprintln!("{}: {}", p["action_type"], p["verdict"]);
                    }
                    eprintln!("({} rules)", list.len());
                }
                OutputMode::Json => println!("{}", serde_json::json!(list)),
            }
        }
        commands::policy::PolicyAction::Clear => {
            write_policies(&std::collections::HashMap::new());
            match output_mode {
                OutputMode::Human => eprintln!("OK"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
        }
    }
    Ok(())
}

async fn tabs(
    cdp: &CdpClient,
    args: commands::tabs::TabsArgs,
    output_mode: OutputMode,
) -> Result<()> {
    match args.action {
        None => {
            // List tabs
            let targets = cdp.get_targets().await?;
            let pages: Vec<_> = targets
                .iter()
                .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
                .collect();
            match output_mode {
                OutputMode::Human => {
                    for t in &pages {
                        let title = t.get("title").and_then(|v| v.as_str()).unwrap_or("");
                        let url = t.get("url").and_then(|v| v.as_str()).unwrap_or("");
                        let id = t.get("targetId").and_then(|v| v.as_str()).unwrap_or("");
                        eprintln!("  [{id}] {title} — {url}");
                    }
                }
                OutputMode::Json => println!("{}", serde_json::json!(pages)),
            }
        }
        Some(commands::tabs::TabAction::New { url }) => {
            let result = cdp
                .send("Target.createTarget", Some(serde_json::json!({"url": url})))
                .await?;
            let target_id = result
                .get("targetId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match output_mode {
                OutputMode::Human => eprintln!("New tab: {target_id}"),
                OutputMode::Json => println!(
                    "{}",
                    serde_json::json!({"success": true, "targetId": target_id})
                ),
            }
        }
        Some(commands::tabs::TabAction::Switch { tab_id }) => {
            cdp.send(
                "Target.activateTarget",
                Some(serde_json::json!({"targetId": tab_id})),
            )
            .await?;
            match output_mode {
                OutputMode::Human => eprintln!("Switched to {tab_id}"),
                OutputMode::Json => println!("{}", serde_json::json!({"success": true})),
            }
        }
        Some(commands::tabs::TabAction::Close { tab_id }) => {
            cdp.send(
                "Target.closeTarget",
                Some(serde_json::json!({"targetId": tab_id})),
            )
            .await?;
            match output_mode {
                OutputMode::Human => eprintln!("Closed {tab_id}"),
                OutputMode::Json => println!("{}", serde_json::json!({"success": true})),
            }
        }
        Some(commands::tabs::TabAction::Find { url: pattern }) => {
            let targets = cdp.get_targets().await?;
            let pattern_str = pattern.replace('*', "");
            if let Some(t) = targets.iter().find(|t| {
                t.get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .contains(&pattern_str)
            }) {
                let tid = t.get("targetId").and_then(|v| v.as_str()).unwrap_or("");
                cdp.send(
                    "Target.activateTarget",
                    Some(serde_json::json!({"targetId": tid})),
                )
                .await?;
                match output_mode {
                    OutputMode::Human => eprintln!("Switched to {tid}"),
                    OutputMode::Json => {
                        println!("{}", serde_json::json!({"success": true, "targetId": tid}))
                    }
                }
            } else {
                anyhow::bail!("No tab matching '{pattern}'");
            }
        }
    }
    Ok(())
}

async fn device(
    cdp: &CdpClient,
    args: commands::device::DeviceArgs,
    output_mode: OutputMode,
) -> Result<()> {
    match args.action {
        commands::device::DeviceAction::Set {
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
                    Some(serde_json::json!({
                        "userAgent": ua,
                    })),
                )
                .await?;
            }
            match output_mode {
                OutputMode::Human => {
                    eprintln!("Device: {width}x{height} (mobile={mobile}, scale={scale})")
                }
                OutputMode::Json => println!(
                    "{}",
                    serde_json::json!({"success": true, "width": width, "height": height, "mobile": mobile})
                ),
            }
        }
        commands::device::DeviceAction::Preset { name } => {
            let (w, h, mobile, scale, ua) = match name.to_lowercase().as_str() {
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
                _ => anyhow::bail!(
                    "Unknown preset '{name}'. Available: iphone-15, iphone-15-pro, pixel-8, ipad-pro, galaxy-s24"
                ),
            };
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
                Some(serde_json::json!({
                    "userAgent": ua,
                })),
            )
            .await?;
            match output_mode {
                OutputMode::Human => eprintln!("Device: {name} ({w}x{h})"),
                OutputMode::Json => println!(
                    "{}",
                    serde_json::json!({"success": true, "preset": name, "width": w, "height": h})
                ),
            }
        }
        commands::device::DeviceAction::Reset => {
            cdp.send("Emulation.clearDeviceMetricsOverride", None)
                .await?;
            cdp.send(
                "Emulation.setUserAgentOverride",
                Some(serde_json::json!({"userAgent": ""})),
            )
            .await?;
            match output_mode {
                OutputMode::Human => eprintln!("Device emulation cleared"),
                OutputMode::Json => println!("{{\"success\":true}}"),
            }
        }
    }
    Ok(())
}

async fn profile(
    cdp: &CdpClient,
    args: commands::profile::ProfileArgs,
    output_mode: OutputMode,
) -> Result<()> {
    if let Some(ref url) = args.url {
        cdp.navigate(url).await?;
    }

    cdp.send("Profiler.enable", None).await?;
    cdp.send("Profiler.start", None).await?;
    eprintln!("Profiling for {} seconds...", args.duration);
    tokio::time::sleep(std::time::Duration::from_secs(args.duration)).await;
    let result = cdp.send("Profiler.stop", None).await?;
    cdp.send("Profiler.disable", None).await?;

    let profile_data = result.get("profile").cloned().unwrap_or_default();
    let output_dir = std::path::Path::new(webpilot::OUTPUT_DIR);
    std::fs::create_dir_all(output_dir)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = output_dir.join(format!("profile_{ts}.cpuprofile"));
    std::fs::write(&path, serde_json::to_string(&profile_data)?)?;

    match output_mode {
        OutputMode::Human => eprintln!("Profile saved: {}", path.display()),
        OutputMode::Json => println!("{}", serde_json::json!({"path": path.to_string_lossy()})),
    }
    Ok(())
}

async fn record(
    cdp: &CdpClient,
    args: commands::record::RecordArgs,
    output_mode: OutputMode,
) -> Result<()> {
    if let Some(ref url) = args.url {
        cdp.navigate(url).await?;
    }

    let frame_count = if let Some(f) = args.frames {
        f
    } else if let Some(d) = args.duration {
        ((d as f64 / args.interval as f64).ceil() as u32).max(1)
    } else {
        anyhow::bail!("Specify --frames or --duration");
    };

    let output_dir = std::path::Path::new(webpilot::OUTPUT_DIR);
    std::fs::create_dir_all(output_dir)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let mut interval =
        tokio::time::interval(std::time::Duration::from_millis(args.interval as u64));
    let mut frames = Vec::new();

    for i in 0..frame_count {
        interval.tick().await;
        let frame_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        let b64 = cdp.screenshot().await?;
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64)?;
        let path = output_dir.join(format!("frame_{ts}_{i:03}.png"));
        std::fs::write(&path, &bytes)?;

        let mut frame = serde_json::json!({
            "index": i,
            "screenshot": path.to_string_lossy(),
            "timestamp_ms": frame_ts as u64,
        });

        if args.dom {
            let dom = call_bridge(
                cdp,
                &serde_json::json!({"type": "extractDOM", "options": {}}).to_string(),
            )
            .await?;
            frame["dom"] = dom;
        }

        frames.push(frame);

        if output_mode == OutputMode::Human {
            eprint!("\rFrame {}/{}", i + 1, frame_count);
        }
    }

    if output_mode == OutputMode::Human {
        eprintln!("\n{} frames -> {}", frame_count, output_dir.display());
    }

    match output_mode {
        OutputMode::Human => {}
        OutputMode::Json => println!(
            "{}",
            serde_json::json!({"frames": frames, "count": frame_count, "interval_ms": args.interval})
        ),
    }
    Ok(())
}
