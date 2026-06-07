//! Test-only helpers: an in-process mock CDP server over a loopback WebSocket,
//! so tests drive the real `CdpClient` (its reader task, id routing, timeout,
//! and teardown) instead of a reimplementation.

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

/// How the mock server reacts to one request.
pub(crate) enum Reply {
    /// Send this full `{ id, ... }` response value.
    Send(Value),
    /// Read the request but never reply — simulates a hung call.
    Silent,
    /// Close the connection after reading the request.
    Close,
}

/// Bind a loopback listener and return it with its `ws://` URL.
pub(crate) async fn bind() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    (listener, url)
}

/// A `{ id, result }` CDP response echoing the request's id.
pub(crate) fn ok(req: &Value, result: Value) -> Value {
    serde_json::json!({ "id": req["id"], "result": result })
}

/// Spawn a mock CDP server that answers each request via `handler`. Returns the
/// `ws://` URL to hand to `CdpClient::connect`.
pub(crate) async fn mock_cdp<F>(handler: F) -> String
where
    F: Fn(&Value) -> Reply + Send + 'static,
{
    let (listener, url) = bind().await;
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut ws) = accept_async(stream).await else {
            return;
        };
        while let Some(Ok(Message::Text(text))) = ws.next().await {
            let Ok(req) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            match handler(&req) {
                Reply::Send(v) => {
                    let _ = ws.send(Message::Text(v.to_string().into())).await;
                }
                Reply::Silent => {}
                Reply::Close => break,
            }
        }
    });
    url
}
