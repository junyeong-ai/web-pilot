//! Unix Domain Socket IPC between CLI (client) and Host (server).

use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Default IPC round-trip timeout (matches host-side `WEBPILOT_IPC_TIMEOUT_MS`).
const DEFAULT_IPC_TIMEOUT_MS: u64 = 60_000;

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

/// Get the socket path.
/// Prefers WEBPILOT_SOCKET env var, then XDG_RUNTIME_DIR (mode 0700), then /tmp.
pub fn socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("WEBPILOT_SOCKET") {
        return PathBuf::from(path);
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join(format!("webpilot-{user}.sock"))
}

fn ipc_timeout() -> Duration {
    std::env::var("WEBPILOT_IPC_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_millis(DEFAULT_IPC_TIMEOUT_MS))
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

/// Send a request to the host and receive a response (CLI side).
pub async fn send_request(request: &serde_json::Value) -> Result<serde_json::Value, IpcError> {
    send_to(&socket_path(), request).await
}

/// Send a request to a specific socket path.
pub async fn send_request_to(
    path: &Path,
    request: &serde_json::Value,
) -> Result<serde_json::Value, IpcError> {
    send_to(path, request).await
}

/// Start the IPC server (Host side). Returns the listener.
pub async fn start_server() -> Result<UnixListener, IpcError> {
    let path = socket_path();

    // Clean up stale socket
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }

    let listener = UnixListener::bind(&path)?;

    // Set socket permissions to owner-only (0600) for security
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    tracing::info!(path = %path.display(), "IPC server listening");
    Ok(listener)
}
