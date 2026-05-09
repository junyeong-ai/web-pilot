use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

#[derive(Args)]
pub struct SessionArgs {
    #[command(subcommand)]
    pub command: SessionCommand,
}

#[derive(Subcommand)]
pub enum SessionCommand {
    /// Export cookies + localStorage to file.
    Export {
        #[arg(long)]
        output: Option<String>,
    },
    /// Import session state from file.
    Import { path: String },
}

pub async fn run<T: Transport>(transport: &mut T, args: SessionArgs) -> Result<CommandOutput> {
    match args.command {
        SessionCommand::Export { output } => {
            let result = transport.send(Command::SessionExport).await?;
            match result {
                ResponseData::SessionExport { path } => {
                    let final_path = if let Some(dest) = output {
                        let dest = std::path::PathBuf::from(dest);
                        if let Some(parent) = dest.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        std::fs::rename(&path, &dest)
                            .or_else(|_| std::fs::copy(&path, &dest).map(|_| ()))
                            .context("Cannot move session file to --output")?;
                        let _ = std::fs::remove_file(&path);
                        dest.to_string_lossy().into_owned()
                    } else {
                        path
                    };
                    Ok(CommandOutput::Data {
                        json: serde_json::json!({"path": final_path}),
                        human: format!("Session exported: {final_path}"),
                    })
                }
                ResponseData::Error { error } => Err(error.into()),
                _ => anyhow::bail!("Unexpected response shape"),
            }
        }
        SessionCommand::Import { path } => {
            let data = std::fs::read_to_string(&path).context("Cannot read session file")?;
            let result = transport.send(Command::SessionImport { data }).await?;
            match result {
                ResponseData::SessionResult { success, error } => {
                    lift_error(success, error, CommandOutput::Ok("Session imported".into()))
                }
                ResponseData::Error { error } => Err(error.into()),
                _ => anyhow::bail!("Unexpected response shape"),
            }
        }
    }
}
