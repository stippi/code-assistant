//! Common streaming infrastructure for LLM providers
//!
//! This module provides shared abstractions for handling streaming responses
//! from LLM providers, supporting both real HTTP responses and recorded
//! playback with identical processing logic.

use crate::recording::RecordedChunk;
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
        match self.response.chunk().await {
            Ok(Some(chunk)) => Ok(Some(chunk.to_vec())),
            Ok(None) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("HTTP chunk error: {}", e)),
        }
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

        let sse_line = format!("data: {}\n", chunk.data);
        self.current_index += 1;

        Ok(Some(sse_line.into_bytes()))
    }
}
