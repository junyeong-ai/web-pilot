use crate::cdp::CdpClient;
use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

pub(crate) async fn run(
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
