use crate::cdp::CdpClient;
use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::cookies::CookiesArgs,
    output_mode: OutputMode,
) -> Result<()> {
    match args.command {
        commands::cookies::CookiesCommand::List { url } => {
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
        commands::cookies::CookiesCommand::Get { url, name } => {
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
        commands::cookies::CookiesCommand::Set {
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
        commands::cookies::CookiesCommand::Delete { url, name } => {
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
