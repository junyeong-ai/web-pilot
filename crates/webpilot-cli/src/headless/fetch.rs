use crate::cdp::CdpClient;
use crate::commands;
use crate::output::CommandOutput;
use anyhow::Result;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::fetch::FetchArgs,
) -> Result<CommandOutput> {
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
    let body = result
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let status = result.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
    Ok(CommandOutput::Content {
        stdout: format!("{body}\nHTTP {status}"),
        json: serde_json::json!({"success": true, "status": result.get("status"), "body": result.get("body")}),
    })
}
