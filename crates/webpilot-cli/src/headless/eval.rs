use crate::cdp::CdpClient;
use crate::commands;
use crate::output::CommandOutput;
use anyhow::Result;

pub(crate) async fn run(
    cdp: &CdpClient,
    args: commands::eval::EvalArgs,
) -> Result<CommandOutput> {
    let val = cdp.evaluate(&format!("({})", args.code)).await?;
    let json_str = serde_json::to_string(&val)?;
    Ok(CommandOutput::Content {
        stdout: json_str.clone(),
        json: serde_json::json!({"success": true, "result": json_str}),
    })
}
