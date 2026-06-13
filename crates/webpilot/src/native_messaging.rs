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
/// Chrome caps a single Host→Extension Native Messaging message at 1 MB.
/// Exposed so the host can reject an oversized command up front with a typed
/// error instead of letting the writer drop it and the caller time out.
pub const MAX_WRITE_SIZE: usize = 1024 * 1024;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trips_a_message_with_a_le_length_prefix() {
        let v = serde_json::json!({"id": 1, "method": "ping"});
        let mut buf = Vec::new();
        write_message(&mut buf, &v).unwrap();
        let body_len = serde_json::to_vec(&v).unwrap().len();
        assert_eq!(
            u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize,
            body_len,
            "the 4-byte prefix must be the LE body length"
        );
        assert_eq!(read_message(&mut Cursor::new(buf)).unwrap(), v);
    }

    #[test]
    fn clean_eof_between_messages_is_typed_eof_not_io() {
        // An empty reader (Chrome closed the pipe between messages) is the clean
        // EOF the host exits on — never a truncation Io error.
        let err = read_message(&mut Cursor::new(Vec::new())).unwrap_err();
        assert!(
            matches!(err, NmError::Eof),
            "empty reader must be Eof, got {err:?}"
        );
    }

    #[test]
    fn truncated_body_is_io_not_eof() {
        // A length prefix promising N bytes followed by FEWER is a torn frame
        // mid-message — an Io error from the body `read_exact`, distinct from the
        // clean between-message Eof, so the host tears the connection down instead
        // of treating a partial frame as a graceful close.
        let mut framed = 10u32.to_le_bytes().to_vec();
        framed.extend_from_slice(b"abc"); // 3 < 10
        let err = read_message(&mut Cursor::new(framed)).unwrap_err();
        assert!(
            matches!(err, NmError::Io(_)),
            "truncated body must be Io, got {err:?}"
        );
    }

    #[test]
    fn oversized_length_is_rejected_before_allocating() {
        // A length prefix exceeding MAX_READ_SIZE is rejected on the 4-byte header
        // ALONE — never `vec![0u8; len]` for an attacker-chosen huge len. The
        // frame carries no body, so a TooLarge here proves the check fires before
        // the body read (and thus before any allocation).
        let framed = ((MAX_READ_SIZE + 1) as u32).to_le_bytes().to_vec();
        let err = read_message(&mut Cursor::new(framed)).unwrap_err();
        assert!(
            matches!(err, NmError::TooLarge(n) if n == MAX_READ_SIZE + 1),
            "oversized len must be TooLarge, got {err:?}"
        );
    }

    #[test]
    fn zero_length_frame_is_a_json_error_not_silent_success() {
        // A zero-length frame has no JSON body — `from_slice(b"")` fails as a JSON
        // error (the host loop breaks), never a silent success the host would act
        // on as an empty command.
        let framed = 0u32.to_le_bytes().to_vec();
        let err = read_message(&mut Cursor::new(framed)).unwrap_err();
        assert!(
            matches!(err, NmError::Json(_)),
            "zero-length frame must be a Json error, got {err:?}"
        );
    }

    #[test]
    fn write_rejects_oversized_payload_before_writing_any_byte() {
        // A payload over Chrome's 1 MB Host→Extension cap is rejected as TooLarge
        // BEFORE any byte reaches the writer, so the host surfaces a typed error
        // up front instead of the writer silently dropping it (and the caller
        // timing out).
        let v = serde_json::json!("x".repeat(MAX_WRITE_SIZE)); // quotes push it past the cap
        let mut buf = Vec::new();
        let err = write_message(&mut buf, &v).unwrap_err();
        assert!(
            matches!(err, NmError::TooLarge(_)),
            "oversized write must be TooLarge, got {err:?}"
        );
        assert!(
            buf.is_empty(),
            "no byte may be written when the payload is rejected"
        );
    }
}
