use crate::cdp::CdpClient;
use crate::output::CommandOutput;
use anyhow::Result;

pub(crate) async fn check(context: Option<&str>) -> Result<CommandOutput> {
    let Some(ws_url) = crate::session::get_existing_session() else {
        return Ok(CommandOutput::Data {
            json: serde_json::json!({"connected": false, "mode": "headless"}),
            human: "Mode: headless\nStatus: no active session".into(),
        });
    };

    let Ok(browser) = CdpClient::connect(&ws_url).await else {
        return Ok(CommandOutput::Data {
            json: serde_json::json!({"connected": false, "mode": "headless"}),
            human: "Mode: headless\nStatus: disconnected".into(),
        });
    };

    // If --context is specified, resolve that context's page target
    let page_target = if let Some(ctx_name) = context {
        let file_path = super::context::context_file_path(ctx_name);
        if let Ok(data) = std::fs::read_to_string(&file_path)
            && let Ok(entry) =
                serde_json::from_str::<super::context::ContextEntry>(&data)
        {
            let targets = browser.get_targets().await?;
            targets.into_iter().find(|t| {
                t.get("targetId").and_then(|v| v.as_str()) == Some(&entry.target_id)
            })
        } else {
            return Ok(CommandOutput::Data {
                json: serde_json::json!({"connected": true, "mode": "headless", "context_error": format!("Context '{}' not found", ctx_name)}),
                human: format!("Mode: headless\nContext '{ctx_name}' not found"),
            });
        }
    } else {
        let targets = browser.get_targets().await?;
        targets
            .into_iter()
            .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
    };

    if let Some(pt) = page_target {
        let target_id = pt.get("targetId").and_then(|v| v.as_str()).unwrap_or("");
        let authority = ws_url.split("/devtools/").next().unwrap_or(&ws_url);
        let page_ws_url = format!("{authority}/devtools/page/{target_id}");
        let cdp = CdpClient::connect(&page_ws_url).await?;
        cdp.send("Runtime.enable", None).await?;
        run(&cdp, context).await
    } else {
        Ok(CommandOutput::Data {
            json: serde_json::json!({"connected": true, "mode": "headless"}),
            human: "Mode: headless\nStatus: connected (no page tab)".into(),
        })
    }
}

pub(crate) async fn run(cdp: &CdpClient, context: Option<&str>) -> Result<CommandOutput> {
    let version = cdp.send("Browser.getVersion", None).await?;
    let title = cdp
        .evaluate("document.title")
        .await
        .unwrap_or(serde_json::Value::Null);
    let url = cdp
        .evaluate("location.href")
        .await
        .unwrap_or(serde_json::Value::Null);

    let mut human = String::from("Mode: headless");
    if let Some(ctx) = context {
        human.push_str(&format!("\nContext: {ctx}"));
    }
    human.push_str(&format!(
        "\nChrome: {}",
        version
            .get("product")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
    ));
    if let Some(t) = title.as_str() {
        human.push_str(&format!("\nTab: {t}"));
    }
    if let Some(u) = url.as_str() {
        human.push_str(&format!("\nURL: {u}"));
    }

    let mut json = serde_json::json!({
        "connected": true,
        "mode": "headless",
        "chrome_version": version.get("product"),
        "tab_title": title,
        "tab_url": url,
    });
    if let Some(ctx) = context {
        json["context"] = serde_json::json!(ctx);
    }

    Ok(CommandOutput::Data { json, human })
}
