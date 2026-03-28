//! Unix Domain Socket IPC between CLI (client) and Host (server).

use std::path::PathBuf;
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
}

/// Get the socket path. WEBPILOT_SOCKET env var overrides the default.
pub fn socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("WEBPILOT_SOCKET") {
        return PathBuf::from(path);
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    PathBuf::from(format!("/tmp/webpilot-{user}.sock"))
}

/// Send a request to the host and receive a response (CLI side).
pub async fn send_request(request: &serde_json::Value) -> Result<serde_json::Value, IpcError> {
    let path = socket_path();
    if !path.exists() {
        return Err(IpcError::HostNotRunning(path.display().to_string()));
    }

    let stream = UnixStream::connect(&path).await?;
    let (reader, mut writer) = stream.into_split();

    // Send request as newline-delimited JSON
    let mut payload = serde_json::to_vec(request)?;
    payload.push(b'\n');
    writer.write_all(&payload).await?;
    writer.shutdown().await?;

    // Read response
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(IpcError::ConnectionClosed);
    }

    let response = serde_json::from_str(line.trim())?;
    Ok(response)
}

/// Send a request to a specific socket path.
pub async fn send_request_to(
    path: &std::path::Path,
    request: &serde_json::Value,
) -> Result<serde_json::Value, IpcError> {
    if !path.exists() {
        return Err(IpcError::HostNotRunning(path.display().to_string()));
    }

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
}

/// Start the IPC server (Host side). Returns the listener.
pub async fn start_server() -> Result<UnixListener, IpcError> {
    let path = socket_path();

    // Clean up stale socket
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }

    let listener = UnixListener::bind(&path)?;
    tracing::info!(path = %path.display(), "IPC server listening");
    Ok(listener)
}
