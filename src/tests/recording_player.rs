//! Module for loading and replaying recorded LLM sessions
//!
//! This module allows loading sessions that were recorded using the APIRecorder
//! and playing them back using a mocked LLM provider, which is useful for testing
//! and for UI demonstrations.

use crate::llm::{
    recording::{RecordedChunk, RecordingSession},
    LLMProvider, LLMRequest, LLMResponse, StreamingCallback, StreamingChunk,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

/// A player for recorded LLM sessions
pub struct RecordingPlayer {
    sessions: Vec<RecordingSession>,
}

impl RecordingPlayer {
    /// Load recordings from a JSON file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let mut file = File::open(path).context("Failed to open recording file")?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .context("Failed to read recording file")?;

        let sessions: Vec<RecordingSession> =
            serde_json::from_str(&contents).context("Failed to parse recording file")?;

        Ok(Self { sessions })
    }

    /// Get a specific session by index
    pub fn get_session(&self, index: usize) -> Option<&RecordingSession> {
        self.sessions.get(index)
    }

    /// Get the number of available sessions
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Create a mock LLM provider that will replay a specific recorded session
    pub fn create_mock_provider(&self, session_index: usize) -> Result<RecordingMockProvider> {
        let session = self
            .get_session(session_index)
            .context("Session index out of bounds")?;

        Ok(RecordingMockProvider {
            session: session.clone(),
            simulate_timing: true,
        })
    }
}

/// A mock LLM provider that replays a recorded session
#[derive(Clone)]
pub struct RecordingMockProvider {
    session: RecordingSession,
    simulate_timing: bool,
}

impl RecordingMockProvider {
    /// Set whether to simulate the original timing between chunks
    pub fn set_simulate_timing(&mut self, simulate: bool) {
        self.simulate_timing = simulate;
    }
}

#[async_trait]
impl LLMProvider for RecordingMockProvider {
    async fn send_message(
        &self,
        _request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // When streaming is requested, replay the recorded chunks with timing
        if let Some(callback) = streaming_callback {
            // Extract the LLM response from the last event
            let response = if let Some(last_chunk) = self.session.chunks.last() {
                parse_response_from_chunk(last_chunk)?
            } else {
                // Return empty response if no chunks are present
                LLMResponse::default()
            };

            // Prepare for streaming with timing information
            let _start = Instant::now();
            let mut last_chunk_time = 0;

            // Stream each chunk with appropriate timing
            for chunk in &self.session.chunks {
                // Simulate the timing between chunks
                if self.simulate_timing && chunk.timestamp_ms > last_chunk_time {
                    let delay = chunk.timestamp_ms - last_chunk_time;
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                last_chunk_time = chunk.timestamp_ms;

                // Extract streaming content and send via callback
                if let Some(streaming_chunk) = parse_streaming_chunk_from_data(&chunk.data)? {
                    callback(&streaming_chunk)?;
                }
            }

            // Return the final response
            Ok(response)
        } else {
            // For non-streaming requests, just return the final response
            if let Some(last_chunk) = self.session.chunks.last() {
                parse_response_from_chunk(last_chunk)
            } else {
                // Return empty response if no chunks are present
                Ok(LLMResponse::default())
            }
        }
    }
}

/// Parse a streaming chunk from a data string
fn parse_streaming_chunk_from_data(data: &str) -> Result<Option<StreamingChunk>> {
    // Parse the event data as JSON
    if let Ok(json) = serde_json::from_str::<Value>(data) {
        // Look for text content in different formats depending on the event type
        if let Some(event_type) = json.get("type").and_then(Value::as_str) {
            match event_type {
                // Content block delta events contain text updates
                "content_block_delta" => {
                    if let Some(delta) = json.get("delta") {
                        // Check for text delta
                        if let Some(text) =
                            delta.get("type").and_then(|t| t.as_str()).and_then(|t| {
                                if t == "text_delta" {
                                    delta.get("text").and_then(Value::as_str)
                                } else if t == "thinking_delta" {
                                    delta.get("thinking").and_then(Value::as_str)
                                } else if t == "input_json_delta" {
                                    delta.get("partial_json").and_then(Value::as_str)
                                } else {
                                    None
                                }
                            })
                        {
                            // Detect content type and create appropriate streaming chunk
                            if delta.get("type").and_then(Value::as_str) == Some("thinking_delta") {
                                return Ok(Some(StreamingChunk::Thinking(text.to_string())));
                            } else if delta.get("type").and_then(Value::as_str)
                                == Some("input_json_delta")
                            {
                                // Get tool information if available
                                let tool_name = json
                                    .get("tool_name")
                                    .and_then(Value::as_str)
                                    .map(String::from);
                                let tool_id = json
                                    .get("tool_id")
                                    .and_then(Value::as_str)
                                    .map(String::from);

                                return Ok(Some(StreamingChunk::InputJson {
                                    content: text.to_string(),
                                    tool_name,
                                    tool_id,
                                }));
                            } else {
                                return Ok(Some(StreamingChunk::Text(text.to_string())));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // If we couldn't extract content, return None
    Ok(None)
}

/// Parse the final LLM response from a chunk
fn parse_response_from_chunk(chunk: &RecordedChunk) -> Result<LLMResponse> {
    use crate::llm::{ContentBlock, Usage};

    // Try to parse the chunk data as JSON
    let json: Value = serde_json::from_str(&chunk.data)?;

    // For simplicity, we create a basic response
    // In a real implementation, we would fully parse the "message_stop" event
    // and look for the corresponding "message_start" to get the full content

    let mut content = vec![];
    let mut usage = Usage {
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
    };

    // Extract usage information if available
    if let Some(usage_obj) = json.get("usage") {
        if let Some(input_tokens) = usage_obj.get("input_tokens").and_then(|v| v.as_u64()) {
            usage.input_tokens = input_tokens as u32;
        }
        if let Some(output_tokens) = usage_obj.get("output_tokens").and_then(|v| v.as_u64()) {
            usage.output_tokens = output_tokens as u32;
        }
    }

    // Add a text content block as a placeholder
    content.push(ContentBlock::Text {
        text: "This content was reconstructed from a recorded session".to_string(),
    });

    Ok(LLMResponse { content, usage })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    // Create a test recording file
    fn create_test_recording() -> Result<(tempfile::TempDir, String)> {
        let dir = tempdir()?;
        let file_path = dir.path().join("test_recording.json");

        // Create a simple recording with one session
        let session = RecordingSession {
            request: serde_json::json!({
                "messages": [
                    {"role": "user", "content": "Hello"}
                ],
                "system": "You are a helpful assistant."
            }),
            timestamp: chrono::Utc::now(),
            chunks: vec![
                RecordedChunk {
                    data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#.to_string(),
                    timestamp_ms: 0,
                },
                RecordedChunk {
                    data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"! How"}}"#.to_string(),
                    timestamp_ms: 500,
                },
                RecordedChunk {
                    data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" can I help you?"}}"#.to_string(),
                    timestamp_ms: 1000,
                },
            ],
        };

        // Create an array with one session
        let sessions = vec![session];

        // Write to file
        let mut file = File::create(&file_path)?;
        file.write_all(serde_json::to_string_pretty(&sessions)?.as_bytes())?;

        Ok((dir, file_path.to_string_lossy().to_string()))
    }

    #[tokio::test]
    async fn test_recording_playback() -> Result<()> {
        // Create a temporary recording file
        let (dir, file_path) = create_test_recording()?;

        // Load the recording
        let player = RecordingPlayer::from_file(file_path)?;

        // Check that we have one session
        assert_eq!(player.session_count(), 1);

        // Create a mock provider
        let mut provider = player.create_mock_provider(0)?;

        // Turn off timing simulation for faster tests
        provider.set_simulate_timing(false);

        // Create a vector to collect chunks
        let chunks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let chunks_clone = chunks.clone();

        // Create a callback to collect chunks
        let callback: StreamingCallback = Box::new(move |chunk: &StreamingChunk| {
            let text = match chunk {
                StreamingChunk::Text(text) => text.clone(),
                StreamingChunk::Thinking(text) => format!("Thinking: {}", text),
                StreamingChunk::InputJson { content, .. } => {
                    format!("JSON: {}", content)
                }
            };
            chunks_clone.lock().unwrap().push(text);
            Ok(())
        });

        // Send a dummy message to trigger playback
        let _ = provider
            .send_message(LLMRequest::default(), Some(&callback))
            .await?;

        // Check collected chunks
        let collected = chunks.lock().unwrap();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0], "Hi");
        assert_eq!(collected[1], "! How");
        assert_eq!(collected[2], " can I help you?");

        // Ensure we remove the temporary directory
        drop(dir);

        Ok(())
    }
}
