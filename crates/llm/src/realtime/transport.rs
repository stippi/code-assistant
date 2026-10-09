//! The WebSocket transport of a realtime session.

use super::{ClientEvent, Incoming, RealtimeConnection, RealtimeConnector, ServerEvent};
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{
        client::IntoClientRequest,
        http::{HeaderName, HeaderValue},
        protocol::Message as WsMessage,
    },
};
use tracing::{debug, info, warn};

/// Where and how to open a realtime session.
#[derive(Clone)]
pub struct RealtimeEndpoint {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

impl std::fmt::Debug for RealtimeEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Header values carry credentials.
        f.debug_struct("RealtimeEndpoint")
            .field("url", &self.url)
            .field(
                "headers",
                &self.headers.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl RealtimeEndpoint {
    /// The endpoint of an OpenAI-compatible API: `base_url` is the HTTP base
    /// (`https://api.openai.com/v1`), the session lives at
    /// `wss://…/v1/realtime?model=<model>`.
    pub fn openai(base_url: &str, api_key: &str, model: &str) -> Self {
        let base = base_url.trim_end_matches('/');
        let ws_base = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            base.to_string()
        };
        Self {
            url: format!("{ws_base}/realtime?model={}", urlencoding::encode(model)),
            headers: vec![("Authorization".to_string(), format!("Bearer {api_key}"))],
        }
    }
}

/// Opens realtime sessions over WebSocket.
pub struct WsConnector {
    endpoint: RealtimeEndpoint,
}

impl WsConnector {
    pub fn new(endpoint: RealtimeEndpoint) -> Self {
        Self { endpoint }
    }
}

#[async_trait::async_trait]
impl RealtimeConnector for WsConnector {
    async fn connect(&self) -> Result<RealtimeConnection> {
        let mut request = self
            .endpoint
            .url
            .as_str()
            .into_client_request()
            .context("Failed to build the realtime WebSocket request")?;
        for (key, value) in &self.endpoint.headers {
            request.headers_mut().insert(
                HeaderName::from_bytes(key.as_bytes()).context("Invalid header name")?,
                HeaderValue::from_str(value).context("Invalid header value")?,
            );
        }
        info!("Connecting realtime session to {}", self.endpoint.url);
        let (stream, _response) = connect_async_with_config(request, None, false)
            .await
            .with_context(|| format!("Realtime connection to {} failed", self.endpoint.url))?;
        let (mut sink, mut source) = stream.split();

        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<ClientEvent>();
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel::<Incoming>();
        let (pong_tx, mut pong_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        // Writer: client events and pongs. Ends when the caller drops its
        // sender, which closes the socket.
        tokio::spawn(async move {
            loop {
                let message = tokio::select! {
                    event = outgoing_rx.recv() => match event {
                        Some(event) => WsMessage::Text(event.to_json().to_string().into()),
                        None => break,
                    },
                    Some(payload) = pong_rx.recv() => WsMessage::Pong(payload.into()),
                };
                if let Err(e) = sink.send(message).await {
                    warn!("Realtime send failed: {e}");
                    return;
                }
            }
            let _ = sink.send(WsMessage::Close(None)).await;
        });

        // Reader: server events until the socket closes.
        tokio::spawn(async move {
            let reason = loop {
                match source.next().await {
                    Some(Ok(WsMessage::Text(text))) => match ServerEvent::parse(&text) {
                        Ok(event) => {
                            if incoming_tx.send(Incoming::Event(event)).is_err() {
                                return;
                            }
                        }
                        Err(e) => debug!("Unparsed realtime event ({e}): {text}"),
                    },
                    Some(Ok(WsMessage::Ping(payload))) => {
                        let _ = pong_tx.send(payload.to_vec());
                    }
                    Some(Ok(WsMessage::Close(frame))) => {
                        break frame.map(|f| format!("{} {}", f.code, f.reason));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => break Some(e.to_string()),
                    None => break None,
                }
            };
            let _ = incoming_tx.send(Incoming::Closed(reason));
        });

        Ok(RealtimeConnection {
            outgoing: outgoing_tx,
            incoming: incoming_rx,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_endpoint_derives_the_websocket_url() {
        let endpoint = RealtimeEndpoint::openai("https://api.openai.com/v1/", "k", "gpt-realtime");
        assert_eq!(
            endpoint.url,
            "wss://api.openai.com/v1/realtime?model=gpt-realtime"
        );
        assert_eq!(endpoint.headers[0].1, "Bearer k");
        assert!(!format!("{endpoint:?}").contains("Bearer"));
    }
}
