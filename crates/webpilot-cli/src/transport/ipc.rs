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
        let operation = command.unconfirmed_operation();
        let request = Request::new(self.ids.next(), command);
        let raw = ipc::send_request(&serde_json::to_value(&request)?)
            .await
            .map_err(|e| connection_error(operation.as_deref(), e))?;
        decode_response(raw, request.id, operation.as_deref())
    }
}

fn connection_error(operation: Option<&str>, source: ipc::IpcError) -> WebPilotError {
    let error = WebPilotError::ConnectionLost {
        detail: format!("Native Messaging host unreachable: {source}"),
    };
    match (operation, source) {
        (Some(operation), ipc::IpcError::Unconfirmed { .. }) => {
            error.with_unknown_outcome(operation)
        }
        _ => error,
    }
}

fn decode_response(
    raw: serde_json::Value,
    request_id: u32,
    operation: Option<&str>,
) -> Result<ResponseData> {
    let response = serde_json::from_value::<Response>(raw)
        .map_err(|e| format!("malformed reply from the Native Messaging host: {e}"))
        .and_then(|response| {
            if response.id == request_id {
                Ok(response)
            } else {
                Err(format!(
                    "Native Messaging reply id {} does not match request {request_id}",
                    response.id
                ))
            }
        })
        .map_err(|detail| {
            let error = WebPilotError::ConnectionLost { detail };
            match operation {
                Some(operation) => error.with_unknown_outcome(operation),
                None => error,
            }
        })?;
    Ok(response.result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_unconfirmed_effects_require_observation() {
        for command in [
            json!({"type":"Fetch", "url":"https://example.com", "method":"POST"}),
            json!({"type":"DomSet", "selector":"input", "property":{"kind":"text"}, "value":"x"}),
            json!({"type":"CookieSet", "url":"https://example.com", "name":"session", "value":"x"}),
            json!({"type":"SessionImport", "data":"{}"}),
            json!({"type":"TabNew", "url":"https://example.com"}),
            json!({"type":"FrameSwitch", "selector":{"by":"main"}}),
        ] {
            let command: Command = serde_json::from_value(command).unwrap();
            let operation = command.unconfirmed_operation();
            assert!(operation.is_some());
            let error = connection_error(
                operation.as_deref(),
                ipc::IpcError::Unconfirmed {
                    source: Box::new(ipc::IpcError::ConnectionClosed),
                },
            );
            assert!(matches!(error, WebPilotError::OutcomeUnknown { .. }));
            assert!(matches!(
                connection_error(
                    operation.as_deref(),
                    ipc::IpcError::HostNotRunning("missing".into())
                ),
                WebPilotError::ConnectionLost { .. }
            ));
        }
        assert!(matches!(
            connection_error(
                Command::TabList.unconfirmed_operation().as_deref(),
                ipc::IpcError::Unconfirmed {
                    source: Box::new(ipc::IpcError::Timeout),
                }
            ),
            WebPilotError::ConnectionLost { .. }
        ));
    }

    #[test]
    fn replies_must_identify_the_dispatched_request() {
        assert!(matches!(
            decode_response(json!({"id":7,"result":{"type":"Pong"}}), 7, None).unwrap(),
            ResponseData::Pong
        ));
        for reply in [
            json!({"id":8,"result":{"type":"Pong"}}),
            json!({"id":7,"result":{}}),
        ] {
            let error = decode_response(reply, 7, Some("fetch")).unwrap_err();
            assert!(matches!(
                error.downcast_ref::<WebPilotError>(),
                Some(WebPilotError::OutcomeUnknown { .. })
            ));
        }
    }
}
