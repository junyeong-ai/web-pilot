//! Chrome Native Messaging protocol: 4-byte little-endian length prefix + JSON payload.

use std::io::{self, Read, Write};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NmError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("message too large: {0} bytes")]
    TooLarge(usize),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("EOF: native messaging connection closed")]
    Eof,
}

// The limits are asymmetric by design. Extension→Host (read) carries tile and
// screenshot payloads and may be large. Host→Extension (write) is capped by
// Chrome at 1 MB; enforcing it here turns silent truncation into a clear error.
const MAX_READ_SIZE: usize = 100 * 1024 * 1024;
const MAX_WRITE_SIZE: usize = 1024 * 1024;

/// Read one NM message from stdin (blocking).
pub fn read_message<R: Read>(reader: &mut R) -> Result<serde_json::Value, NmError> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(NmError::Eof),
        Err(e) => return Err(NmError::Io(e)),
    }

    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_READ_SIZE {
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
    if len > MAX_WRITE_SIZE {
        return Err(NmError::TooLarge(len));
    }

    writer.write_all(&(len as u32).to_le_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}
