//! Unix Domain Socket IPC between CLI (client) and Host (server).

use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("host not running (socket not found at {0})")]
    HostNotRunning(String),
    #[error("connection closed")]
    ConnectionClosed,
    #[error("timed out")]
    Timeout,
}

pub fn socket_path() -> PathBuf {
    crate::dirs::socket_path()
}

fn ipc_timeout() -> Duration {
    crate::settings::timeouts().ipc_response
}

async fn send_to(path: &Path, request: &serde_json::Value) -> Result<serde_json::Value, IpcError> {
    if !path.exists() {
        return Err(IpcError::HostNotRunning(path.display().to_string()));
    }

    match tokio::time::timeout(ipc_timeout(), async {
        let stream = UnixStream::connect(path).await?;
        let (reader, mut writer) = stream.into_split();

        let mut payload = serde_json::to_vec(request)?;
        payload.push(b'\n');
        writer.write_all(&payload).await?;
        writer.shutdown().await?;

        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(IpcError::ConnectionClosed);
        }

        let response = serde_json::from_str(line.trim())?;
        Ok(response)
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(IpcError::Timeout),
    }
}

pub async fn send_request(request: &serde_json::Value) -> Result<serde_json::Value, IpcError> {
    send_to(&socket_path(), request).await
}

/// Bind the IPC server (Host side). Removes any stale socket file.
pub async fn start_server() -> Result<UnixListener, IpcError> {
    let path = socket_path();

    // Ensure parent dir exists with restrictive perms.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }

    // Clear a stale socket — and ONLY a socket. The path derives from the
    // (env-overridable) runtime root, so unlinking whatever sits there would
    // let a mispointed WEBPILOT_HOME delete a file we do not own.
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        use std::os::unix::fs::FileTypeExt;
        if !meta.file_type().is_socket() {
            return Err(IpcError::Io(std::io::Error::other(format!(
                "refusing to replace non-socket file at {}",
                path.display()
            ))));
        }
        let _ = std::fs::remove_file(&path);
    }

    let listener = UnixListener::bind(&path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    tracing::info!(path = %path.display(), "IPC server listening");
    Ok(listener)
}
