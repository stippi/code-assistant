//! Common streaming infrastructure for LLM providers
//!
//! This module provides shared abstractions for handling streaming responses
//! from LLM providers, supporting both real HTTP responses and recorded
//! playback with identical processing logic.

use crate::recording::RecordedChunk;
use crate::utils::{STREAM_IDLE_TIMEOUT, error_chain};
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

/// Real HTTP response chunk stream
pub struct HttpChunkStream {
    pub response: Response,
}

impl HttpChunkStream {
    pub fn new(response: Response) -> Self {
        Self { response }
    }
}

#[async_trait]
impl ChunkStream for HttpChunkStream {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        next_http_chunk(&mut self.response).await
    }
}

/// Read the next chunk of a streaming response, failing when the stream stays
/// silent for longer than [`STREAM_IDLE_TIMEOUT`].
///
/// Errors here are plain (not `ApiErrorContext`) on purpose: part of the answer
/// may already have been streamed to the UI, and a retry would repeat it.
pub async fn next_http_chunk(response: &mut Response) -> Result<Option<Vec<u8>>> {
    next_http_chunk_within(response, STREAM_IDLE_TIMEOUT).await
}

async fn next_http_chunk_within(
    response: &mut Response,
    idle_timeout: Duration,
) -> Result<Option<Vec<u8>>> {
    match tokio::time::timeout(idle_timeout, response.chunk()).await {
        Ok(Ok(chunk)) => Ok(chunk.map(|chunk| chunk.to_vec())),
        Ok(Err(e)) => Err(anyhow::anyhow!(
            "LLM stream interrupted: {}",
            error_chain(&e)
        )),
        Err(_) => Err(anyhow::anyhow!(
            "LLM stream stalled: no data received for {}s",
            idle_timeout.as_secs()
        )),
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

    /// Answers one request with the headers and a first chunk, then keeps the
    /// connection open without sending anything more.
    async fn stalling_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
            drop(socket);
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn stalled_stream_fails_instead_of_waiting_forever() {
        let url = stalling_server().await;
        let mut response = reqwest::get(url).await.unwrap();
        let idle = Duration::from_millis(200);

        let first = next_http_chunk_within(&mut response, idle).await.unwrap();
        assert_eq!(first.as_deref(), Some(&b"hello"[..]));

        let error = next_http_chunk_within(&mut response, idle)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("stalled"), "{error}");
    }
}
