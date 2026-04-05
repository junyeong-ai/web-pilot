use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::ipc;
use webpilot::protocol::{Command, ResponseData};

use crate::output::OutputMode;

#[derive(Args)]
pub struct SessionArgs {
    #[command(subcommand)]
    pub command: SessionCommand,
}

#[derive(Subcommand)]
pub enum SessionCommand {
    /// Export cookies + localStorage to file
    Export {
        /// Output file path
        #[arg(long)]
        output: Option<String>,
    },
    /// Import session state from file
    Import {
        /// Input file path
        path: String,
    },
}

pub async fn run(args: SessionArgs, output_mode: OutputMode) -> Result<()> {
    match args.command {
        SessionCommand::Export { output } => {
            let request =
                serde_json::to_value(webpilot::protocol::Request::new(1, Command::ExportSession))?;
            let response = ipc::send_request(&request)
                .await
                .context("Host not running")?;
            let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

            match resp.result {
                ResponseData::SessionExport { path } => {
                    // If --output specified, move file from default location
                    let final_path = if let Some(ref dest) = output {
                        let dest = std::path::PathBuf::from(dest);
                        if let Some(parent) = dest.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        std::fs::rename(&path, &dest)
                            .or_else(|_| std::fs::copy(&path, &dest).map(|_| ()))
                            .context("Cannot move session file to --output path")?;
                        let _ = std::fs::remove_file(&path);
                        dest.to_string_lossy().to_string()
                    } else {
                        path
                    };
                    match output_mode {
                        OutputMode::Human => eprintln!("Session exported: {final_path}"),
                        OutputMode::Json => println!("{}", serde_json::json!({"path": final_path})),
                    }
                }
                ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        SessionCommand::Import { path } => {
            let data = std::fs::read_to_string(&path).context("Cannot read session file")?;
            let request = serde_json::to_value(webpilot::protocol::Request::new(
                1,
                Command::ImportSession { data },
            ))?;
            let response = ipc::send_request(&request)
                .await
                .context("Host not running")?;
            let resp: webpilot::protocol::Response = serde_json::from_value(response)?;

            match resp.result {
                ResponseData::SessionResult { success, error } => {
                    match output_mode {
                        OutputMode::Human => {
                            if success {
                                eprintln!("Session imported");
                            } else if let Some(ref err) = error {
                                eprintln!("{}", crate::output::format_error(err));
                            } else {
                                eprintln!("Unknown error");
                            }
                        }
                        OutputMode::Json => println!(
                            "{}",
                            serde_json::json!({"success": success, "error": error})
                        ),
                    }
                    if !success {
                        if let Some(ref err) = error {
                            anyhow::bail!("{}", crate::output::format_error(err));
                        } else {
                            anyhow::bail!("Unknown error");
                        }
                    }
                }
                ResponseData::Error { message, .. } => anyhow::bail!("{message}"),
                _ => anyhow::bail!("Unexpected response"),
            }
        }
    }
    Ok(())
}
