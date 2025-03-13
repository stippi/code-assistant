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
use std::sync::{Arc, Mutex};
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

    /// Get the number of available sessions
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Create a mock LLM provider that will replay all sessions sequentially
    pub fn create_provider(&self) -> Result<RecordingProvider> {
        if self.sessions.is_empty() {
            return Err(anyhow::anyhow!("No sessions available for playback"));
        }

        Ok(RecordingProvider {
            sessions: self.sessions.clone(),
            current_session: Arc::new(Mutex::new(0)),
            simulate_timing: true,
        })
    }
}

/// A mock LLM provider that replays sessions sequentially
#[derive(Clone)]
pub struct RecordingProvider {
    sessions: Vec<RecordingSession>,
    current_session: Arc<Mutex<usize>>,
    simulate_timing: bool,
}

impl RecordingProvider {
    /// Set whether to simulate the original timing between chunks
    pub fn set_simulate_timing(&mut self, simulate: bool) {
        self.simulate_timing = simulate;
    }
    
    /// Get the current session index
    pub fn current_index(&self) -> usize {
        *self.current_session.lock().unwrap()
    }
    
    /// Get the total number of sessions
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

#[async_trait]
impl LLMProvider for RecordingProvider {
    async fn send_message(
        &self,
        _request: LLMRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<LLMResponse> {
        // Get the session for this request and increment the index atomically
        let session_index;
        let session;
        {
            let mut index_guard = self.current_session.lock().unwrap();
            session_index = *index_guard;
            
            // Make sure we don't exceed the available sessions
            if session_index >= self.sessions.len() {
                return Err(anyhow::anyhow!("No more recorded sessions available"));
            }
            
            // Get the session for this request (clone it to avoid borrowing issues)
            session = self.sessions[session_index].clone();
            
            // Increment for next time
            *index_guard = session_index + 1;
            
            // Lock is automatically dropped at the end of this block
        }
        
        // When streaming is requested, replay the recorded chunks with timing
        if let Some(callback) = streaming_callback {
            // Extract the LLM response from the last event
            let response = if let Some(last_chunk) = &session.chunks.last() {
                parse_response_from_chunk(last_chunk)?
            } else {
                // Return empty response if no chunks are present
                LLMResponse::default()
            };

            // Prepare for streaming with timing information
            let _start = Instant::now();
            let mut last_chunk_time = 0;

            // Stream each chunk with appropriate timing
            for chunk in &session.chunks {
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
            if let Some(last_chunk) = session.chunks.last() {
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

    // Create a test recording file with multiple sessions
    fn create_test_recording() -> Result<(tempfile::TempDir, String)> {
        let dir = tempdir()?;
        let file_path = dir.path().join("test_recording.json");

        // Create a simple recording with two sessions
        let sessions = vec![
            RecordingSession {
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
                        data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"! How are you?"}}"#.to_string(),
                        timestamp_ms: 500,
                    },
                ],
            },
            RecordingSession {
                request: serde_json::json!({
                    "messages": [
                        {"role": "user", "content": "What can you do?"}
                    ],
                    "system": "You are a helpful assistant."
                }),
                timestamp: chrono::Utc::now(),
                chunks: vec![
                    RecordedChunk {
                        data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"I can help"}}"#.to_string(),
                        timestamp_ms: 0,
                    },
                    RecordedChunk {
                        data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" with many tasks!"}}"#.to_string(),
                        timestamp_ms: 500,
                    },
                ],
            },
        ];

        // Write to file
        let mut file = File::create(&file_path)?;
        file.write_all(serde_json::to_string_pretty(&sessions)?.as_bytes())?;

        Ok((dir, file_path.to_string_lossy().to_string()))
    }
    
    #[tokio::test]
    async fn test_sequential_playback() -> Result<()> {
        // Create a temporary recording file
        let (dir, file_path) = create_test_recording()?;

        // Load the recording
        let player = RecordingPlayer::from_file(file_path)?;
        
        // Create a provider
        let mut provider = player.create_provider()?;
        
        // Turn off timing simulation for faster tests
        provider.set_simulate_timing(false);
        
        // Verify initial state
        assert_eq!(provider.current_index(), 0);
        assert_eq!(provider.session_count(), 2);
        
        // Create two callbacks with separate chunk collectors
        let first_chunks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let first_chunks_clone = first_chunks.clone();
        
        let first_callback: StreamingCallback = Box::new(move |chunk: &StreamingChunk| {
            if let StreamingChunk::Text(text) = chunk {
                first_chunks_clone.lock().unwrap().push(text.clone());
            }
            Ok(())
        });
        
        let second_chunks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let second_chunks_clone = second_chunks.clone();
        
        let second_callback: StreamingCallback = Box::new(move |chunk: &StreamingChunk| {
            if let StreamingChunk::Text(text) = chunk {
                second_chunks_clone.lock().unwrap().push(text.clone());
            }
            Ok(())
        });
        
        // First request should play the first session
        let _ = provider
            .send_message(LLMRequest::default(), Some(&first_callback))
            .await?;
            
        // Second request should play the second session
        let _ = provider
            .send_message(LLMRequest::default(), Some(&second_callback))
            .await?;
            
        // Check that we incremented the session index
        assert_eq!(provider.current_index(), 2);
        
        // Check collected chunks from first request
        let first_collected = first_chunks.lock().unwrap();
        assert_eq!(first_collected.len(), 2);
        assert_eq!(first_collected[0], "Hi");
        assert_eq!(first_collected[1], "! How are you?");
        
        // Check collected chunks from second request
        let second_collected = second_chunks.lock().unwrap();
        assert_eq!(second_collected.len(), 2);
        assert_eq!(second_collected[0], "I can help");
        assert_eq!(second_collected[1], " with many tasks!");
        
        // A third request should fail since we only have 2 sessions
        let result = provider.send_message(LLMRequest::default(), None).await;
        assert!(result.is_err());
        
        // Ensure we remove the temporary directory
        drop(dir);
        
        Ok(())
    }
}
