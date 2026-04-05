use crate::cdp::CdpClient;
use crate::commands;
use crate::output::OutputMode;
use anyhow::Result;

pub(crate) async fn run(
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
                    eprintln!("{}", crate::output::format_error_str(&e.to_string()))
                }
                OutputMode::Json => println!(
                    "{}",
                    serde_json::json!({"success": false, "error": e.to_string()})
                ),
            }
            anyhow::bail!("{}", crate::output::format_error_str(&e.to_string()));
        }
    }
    Ok(())
}
