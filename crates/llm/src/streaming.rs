//! Common streaming infrastructure for LLM providers
//!
//! This module provides shared abstractions for handling streaming responses
//! from LLM providers, supporting both real HTTP responses and recorded
//! playback with identical processing logic.

use crate::ApiError;
use crate::recording::RecordedChunk;
use crate::utils::{RESPONSE_START_TIMEOUT, STREAM_IDLE_TIMEOUT, error_chain};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Response;
use std::time::{Duration, Instant};

/// Trait for streaming chunk sources (real HTTP response or recorded playback)
///
/// This abstraction allows providers to use the same streaming processing logic
/// for both live HTTP responses and recorded playback, ensuring identical behavior.
#[async_trait]
pub trait ChunkStream: Send {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>>;
}

/// Real HTTP response chunk stream.
///
/// Bounds how long the stream may stay silent: up to the first chunk by
/// [`RESPONSE_START_TIMEOUT`], between later chunks by [`STREAM_IDLE_TIMEOUT`].
/// Providers send an event right away once a response starts (Anthropic's
/// `message_start`, OpenAI's `response.created` and reasoning items, even
/// those without summary), while a model may legitimately go quiet for minutes
/// in the middle of a response, e.g. while reasoning.
pub struct HttpChunkStream {
    response: Response,
    received_any: bool,
    first_chunk_timeout: Duration,
    idle_timeout: Duration,
}

impl HttpChunkStream {
    pub fn new(response: Response) -> Self {
        Self {
            response,
            received_any: false,
            first_chunk_timeout: RESPONSE_START_TIMEOUT,
            idle_timeout: STREAM_IDLE_TIMEOUT,
        }
    }

    /// Wait up to `timeout` for the first chunk instead of
    /// [`RESPONSE_START_TIMEOUT`], for servers that may take long to start
    /// (e.g. a local server loading the model).
    pub fn with_first_chunk_timeout(mut self, timeout: Duration) -> Self {
        self.first_chunk_timeout = timeout;
        self
    }
}

#[async_trait]
impl ChunkStream for HttpChunkStream {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        let timeout = if self.received_any {
            self.idle_timeout
        } else {
            self.first_chunk_timeout
        };
        let error = match tokio::time::timeout(timeout, self.response.chunk()).await {
            Ok(Ok(chunk)) => {
                self.received_any = true;
                return Ok(chunk.map(|chunk| chunk.to_vec()));
            }
            Ok(Err(e)) => format!("LLM stream interrupted: {}", error_chain(&e)),
            Err(_) => format!(
                "LLM stream stalled: no data received for {}s",
                timeout.as_secs()
            ),
        };
        if self.received_any {
            // Part of the answer may already be on screen; a retry would
            // repeat it, so this is a plain (non-retryable) error.
            Err(anyhow::anyhow!(error))
        } else {
            // Nothing was processed yet, so retrying is safe.
            Err(ApiError::NetworkError(error).into())
        }
    }
}

/// Decodes a byte stream as UTF-8 across chunk boundaries.
///
/// Network chunks may end in the middle of a multi-byte character (umlauts,
/// emoji); decoding each chunk on its own would fail the whole request. The
/// incomplete tail is kept and completed by the next chunk.
#[derive(Default)]
pub struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    pub fn decode(&mut self, chunk: &[u8]) -> Result<String> {
        self.pending.extend_from_slice(chunk);
        let valid_up_to = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            // `error_len() == None`: the input ends inside a character.
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(e) => return Err(anyhow::anyhow!("LLM stream is not valid UTF-8: {e}")),
        };
        let rest = self.pending.split_off(valid_up_to);
        let text = String::from_utf8(std::mem::replace(&mut self.pending, rest))?;
        Ok(text)
    }
}

/// Recorded chunk stream for playback
pub struct PlaybackChunkStream {
    chunks: Vec<RecordedChunk>,
    current_index: usize,
    start_time: Instant,
    fast_mode: bool,
}

impl PlaybackChunkStream {
    pub fn new(chunks: Vec<RecordedChunk>, fast_mode: bool) -> Self {
        Self {
            chunks,
            current_index: 0,
            start_time: Instant::now(),
            fast_mode,
        }
    }
}

#[async_trait]
impl ChunkStream for PlaybackChunkStream {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        if self.current_index >= self.chunks.len() {
            return Ok(None);
        }

        let chunk = &self.chunks[self.current_index];

        // Handle timing - either fast playback or respect original timing
        if !self.fast_mode {
            let elapsed = self.start_time.elapsed();
            let expected_time = Duration::from_millis(chunk.timestamp_ms);

            if elapsed < expected_time {
                let sleep_duration = expected_time - elapsed;
                tokio::time::sleep(sleep_duration).await;
            }
        } else {
            // Fast playback - small delay to simulate streaming
            tokio::time::sleep(Duration::from_millis(17)).await; // ~60fps
        }

        let line = format!("{}\n", chunk.data);
        self.current_index += 1;

        Ok(Some(line.into_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn utf8_decoder_joins_characters_split_across_chunks() {
        let bytes = "Grüße 👋".as_bytes();
        // Split inside "ü" (2 bytes) and inside the emoji (4 bytes).
        let (a, rest) = bytes.split_at(3);
        let (b, c) = rest.split_at(rest.len() - 2);

        let mut decoder = Utf8Decoder::default();
        let decoded = [a, b, c]
            .iter()
            .map(|chunk| decoder.decode(chunk).unwrap())
            .collect::<String>();

        assert_eq!(decoded, "Grüße 👋");
        assert_eq!(decoder.decode(b"").unwrap(), "");
    }

    #[test]
    fn utf8_decoder_rejects_invalid_bytes() {
        let mut decoder = Utf8Decoder::default();
        assert!(decoder.decode(&[b'a', 0xff, b'b']).is_err());
    }

    /// Answers one request with the headers (and a first chunk when
    /// `first_chunk`), then keeps the connection open without sending more.
    async fn stalling_server(first_chunk: bool) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let mut response = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
            if first_chunk {
                response.extend_from_slice(b"5\r\nhello\r\n");
            }
            socket.write_all(&response).await.unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
            drop(socket);
        });
        format!("http://{addr}")
    }

    fn stream(response: Response) -> HttpChunkStream {
        HttpChunkStream {
            idle_timeout: Duration::from_millis(200),
            ..HttpChunkStream::new(response).with_first_chunk_timeout(Duration::from_millis(200))
        }
    }

    #[tokio::test]
    async fn stall_after_first_chunk_is_not_retryable() {
        let url = stalling_server(true).await;
        let mut stream = stream(reqwest::get(url).await.unwrap());

        let first = stream.next_chunk().await.unwrap();
        assert_eq!(first.as_deref(), Some(&b"hello"[..]));

        let error = stream.next_chunk().await.unwrap_err();
        assert!(error.to_string().contains("stalled"), "{error}");
        assert!(error.downcast_ref::<ApiError>().is_none());
    }

    #[tokio::test]
    async fn stall_before_first_chunk_is_retryable() {
        let url = stalling_server(false).await;
        let mut stream = stream(reqwest::get(url).await.unwrap());

        let error = stream.next_chunk().await.unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<ApiError>(),
                Some(ApiError::NetworkError(_))
            ),
            "{error}"
        );
    }
}
