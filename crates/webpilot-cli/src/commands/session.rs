use std::io::Read;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use webpilot::protocol::{Command, ResponseData};

use crate::output::CommandOutput;
use crate::transport::{Transport, lift_error};

/// A session file is cookies + storage — kilobytes in practice. Cap the read so
/// a fat-fingered or malicious path can't exhaust memory before the import is
/// even parsed (the native-messaging host rejects oversized payloads downstream,
/// but the CLI must not allocate gigabytes first).
const MAX_SESSION_BYTES: u64 = 64 * 1024 * 1024;

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
                        // The artifact copy holds session secrets — a failed
                        // cleanup must be visible, never silent.
                        if std::path::Path::new(&path).exists()
                            && let Err(e) = std::fs::remove_file(&path)
                        {
                            tracing::warn!("session artifact left at {path}: {e}");
                        }
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
            let file = std::fs::File::open(&path).context("Cannot read session file")?;
            let mut buf = Vec::new();
            file.take(MAX_SESSION_BYTES + 1)
                .read_to_end(&mut buf)
                .context("Cannot read session file")?;
            if buf.len() as u64 > MAX_SESSION_BYTES {
                return Err(webpilot::WebPilotError::InvalidArgument {
                    detail: format!("session file exceeds the {MAX_SESSION_BYTES}-byte limit"),
                }
                .into());
            }
            let data =
                String::from_utf8(buf).map_err(|_| webpilot::WebPilotError::InvalidArgument {
                    detail: "session file is not valid UTF-8".into(),
                })?;
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
