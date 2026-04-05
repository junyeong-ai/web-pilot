use crate::cdp::CdpClient;
use crate::output::OutputMode;
use anyhow::Result;

pub(crate) async fn check(output_mode: OutputMode) -> Result<()> {
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
        run(&cdp, output_mode).await
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

pub(crate) async fn run(cdp: &CdpClient, output_mode: OutputMode) -> Result<()> {
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
