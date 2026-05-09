//! Transport abstraction.
//!
//! Every CLI command — regardless of mode — produces a `protocol::Command`
//! and consumes a `protocol::ResponseData`. The `Transport` trait is the
//! sole boundary between command logic and how those bytes reach Chrome:
//!   - `IpcTransport`   → Unix socket → Native Messaging Host → Extension.
//!   - `LocalTransport` → CDP WebSocket → headless Chrome.
//!
//! With this abstraction, command handlers are written exactly once. Adding
//! a new command means: extend `protocol::Command`, handle it in the `cmd`
//! module, and add the matching arm to each `Transport` impl.

pub mod ipc;
pub mod local;
pub(crate) mod local_context;

use anyhow::Result;
use std::sync::atomic::{AtomicU32, Ordering};
use webpilot::WebPilotError;
use webpilot::protocol::{Command, ResponseData};

pub use ipc::IpcTransport;
pub use local::LocalTransport;

/// Send a typed `Command`, await typed `ResponseData`. Errors are
/// `WebPilotError` — no message-string inspection on the caller side.
pub trait Transport: Send {
    fn send(
        &mut self,
        command: Command,
    ) -> impl std::future::Future<Output = Result<ResponseData>> + Send;
}

/// Convert a per-variant `Option<WebPilotError>` flag into an error if set.
pub fn lift_error<T>(success: bool, error: Option<WebPilotError>, payload: T) -> Result<T> {
    if !success {
        return Err(error
            .map(anyhow::Error::from)
            .unwrap_or_else(|| anyhow::anyhow!("operation failed without error detail")));
    }
    Ok(payload)
}

/// Monotonic ID generator shared by transport implementations.
#[derive(Default)]
pub(crate) struct IdGen(AtomicU32);

impl IdGen {
    pub fn next(&self) -> u32 {
        let prev = self.0.fetch_add(1, Ordering::Relaxed);
        prev.checked_add(1).unwrap_or(1)
    }
}
