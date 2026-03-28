//! Chrome Native Messaging protocol: 4-byte little-endian length prefix + JSON payload.

use std::io::{self, Read, Write};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NmError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("message too large: {0} bytes (max 1MB)")]
    TooLarge(usize),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("EOF: native messaging connection closed")]
    Eof,
}

// Extension→Host messages can be up to 4GB (Chrome enforces 1MB only for Host→Extension)
const MAX_MESSAGE_SIZE: usize = 100 * 1024 * 1024; // 100 MB (practical limit for tile data)

/// Read one NM message from stdin (blocking).
pub fn read_message<R: Read>(reader: &mut R) -> Result<serde_json::Value, NmError> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(NmError::Eof),
        Err(e) => return Err(NmError::Io(e)),
    }

    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_MESSAGE_SIZE {
        return Err(NmError::TooLarge(len));
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;

    let value = serde_json::from_slice(&buf)?;
    Ok(value)
}

/// Write one NM message to stdout (blocking).
pub fn write_message<W: Write>(writer: &mut W, value: &serde_json::Value) -> Result<(), NmError> {
    let payload = serde_json::to_vec(value)?;
    let len = payload.len();
    if len > MAX_MESSAGE_SIZE {
        return Err(NmError::TooLarge(len));
    }

    writer.write_all(&(len as u32).to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}
