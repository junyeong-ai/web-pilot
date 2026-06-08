//! Transport over Unix domain socket → Native Messaging Host.

use anyhow::Result;
use webpilot::WebPilotError;
use webpilot::ipc;
use webpilot::protocol::{Command, Request, Response, ResponseData};

use super::{IdGen, Transport};

pub struct IpcTransport {
    ids: IdGen,
}

impl IpcTransport {
    pub fn new() -> Self {
        Self {
            ids: IdGen::default(),
        }
    }
}

impl Default for IpcTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for IpcTransport {
    async fn send(&mut self, command: Command) -> Result<ResponseData> {
        // Policy is enforced in the Native Messaging host (the process that
        // actually reaches the authenticated browser), not here — the CLI side
        // is just a socket writer and could be bypassed by writing the socket
        // directly. The host returns a `ResponseData::Error { PolicyDenied }`
        // which surfaces below exactly like any other typed failure.
        let request = Request::new(self.ids.next(), command);
        // Every way the host channel can fail — not running, socket closed,
        // timeout, I/O — is infra, so it maps to ConnectionLost (exit 3), the
        // documented bucket an agent retries on, never a generic Other (exit 1).
        let raw = ipc::send_request(&serde_json::to_value(&request)?)
            .await
            .map_err(|e| WebPilotError::ConnectionLost {
                detail: format!("Native Messaging host unreachable: {e}"),
            })?;
        // A host reply that doesn't parse is the host misbehaving — also infra.
        let response: Response =
            serde_json::from_value(raw).map_err(|e| WebPilotError::ConnectionLost {
                detail: format!("malformed reply from the Native Messaging host: {e}"),
            })?;
        Ok(response.result)
    }
}
