use crate::cdp::CdpClient;
use crate::commands;
use crate::output::CommandOutput;
use anyhow::Result;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::cookies::CookiesArgs,
) -> Result<CommandOutput> {
    match args.command {
        commands::cookies::CookieCommand::List { url } => {
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
            let human_lines: Vec<String> = cookies
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|c| {
                            let val = c.get("value").and_then(|v| v.as_str()).unwrap_or("");
                            format!(
                                "{}={} ({})",
                                c.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                                val.get(..20).unwrap_or(val),
                                c.get("domain").and_then(|v| v.as_str()).unwrap_or("")
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(CommandOutput::List {
                items: cookies,
                human_lines,
                summary: String::new(),
            })
        }
        commands::cookies::CookieCommand::Get { url, name } => {
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
                Ok(CommandOutput::Content {
                    stdout: c
                        .get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    json: c.clone(),
                })
            } else {
                anyhow::bail!("Cookie '{name}' not found");
            }
        }
        commands::cookies::CookieCommand::Set {
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
            Ok(CommandOutput::Ok("OK".into()))
        }
        commands::cookies::CookieCommand::Delete { url, name } => {
            cdp.send(
                "Network.deleteCookies",
                Some(serde_json::json!({
                    "url": url,
                    "name": name,
                })),
            )
            .await?;
            Ok(CommandOutput::Ok("OK".into()))
        }
    }
}
