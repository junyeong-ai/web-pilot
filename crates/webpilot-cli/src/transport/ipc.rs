//! Transport over Unix domain socket → Native Messaging Host.

use anyhow::{Context, Result};
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
        let request = Request::new(self.ids.next(), command);
        let raw = ipc::send_request(&serde_json::to_value(&request)?)
            .await
            .context("IPC dispatch failed (host not running?)")?;
        let response: Response = serde_json::from_value(raw)?;
        Ok(response.result)
    }
}
