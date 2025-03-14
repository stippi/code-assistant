//! Module for loading and replaying recorded LLM sessions
//!
//! This module allows loading sessions that were recorded using the APIRecorder
//! and playing them back using a mocked LLM provider, which is useful for testing
//! and for UI demonstrations.

use crate::llm::{
    recording::{RecordedChunk, RecordingSession},
    ContentBlock, LLMProvider, LLMRequest, LLMResponse, StreamingCallback, StreamingChunk, Usage,
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
            // Parse complete response (all content blocks) from session
            let content = build_content_blocks(&session)?;

            // Get usage information
            let initial_usage = parse_initial_response(&session.chunks)?;
            let final_usage = parse_final_usage(&session.chunks)?;

            // Combine usage information
            let usage = Usage {
                input_tokens: initial_usage.input_tokens,
                output_tokens: final_usage.output_tokens,
                cache_creation_input_tokens: initial_usage.cache_creation_input_tokens,
                cache_read_input_tokens: initial_usage.cache_read_input_tokens,
            };

            // Create response
            let response = LLMResponse { content, usage };

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
            // For non-streaming requests, just build and return the final response
            let content = build_content_blocks(&session)?;

            // Get usage information
            let initial_usage = parse_initial_response(&session.chunks)?;
            let final_usage = parse_final_usage(&session.chunks)?;

            // Combine usage information
            let usage = Usage {
                input_tokens: initial_usage.input_tokens,
                output_tokens: final_usage.output_tokens,
                cache_creation_input_tokens: initial_usage.cache_creation_input_tokens,
                cache_read_input_tokens: initial_usage.cache_read_input_tokens,
            };

            // Create response
            Ok(LLMResponse { content, usage })
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

/// Parse an initial LLM response for session tracking
fn parse_initial_response(chunks: &[RecordedChunk]) -> Result<Usage> {
    use crate::llm::Usage;

    // Find the message_start event which has usage info
    for chunk in chunks {
        if let Ok(json) = serde_json::from_str::<Value>(&chunk.data) {
            if json.get("type").and_then(Value::as_str) == Some("message_start") {
                if let Some(message) = json.get("message") {
                    if let Some(usage) = message.get("usage") {
                        let mut result = Usage::default();

                        if let Some(input_tokens) =
                            usage.get("input_tokens").and_then(|v| v.as_u64())
                        {
                            result.input_tokens = input_tokens as u32;
                        }
                        if let Some(output_tokens) =
                            usage.get("output_tokens").and_then(|v| v.as_u64())
                        {
                            result.output_tokens = output_tokens as u32;
                        }
                        if let Some(creation) = usage
                            .get("cache_creation_input_tokens")
                            .and_then(|v| v.as_u64())
                        {
                            result.cache_creation_input_tokens = creation as u32;
                        }
                        if let Some(read) = usage
                            .get("cache_read_input_tokens")
                            .and_then(|v| v.as_u64())
                        {
                            result.cache_read_input_tokens = read as u32;
                        }

                        return Ok(result);
                    }
                }
            }
        }
    }

    // Return default if not found
    Ok(Usage::default())
}

/// Parse final usage information from message_delta event
fn parse_final_usage(chunks: &[RecordedChunk]) -> Result<Usage> {
    use crate::llm::Usage;

    // Go through chunks in reverse to find the message_delta event with usage info
    for chunk in chunks.iter().rev() {
        if let Ok(json) = serde_json::from_str::<Value>(&chunk.data) {
            if json.get("type").and_then(Value::as_str) == Some("message_delta") {
                if let Some(usage) = json.get("usage") {
                    let mut result = Usage::default();

                    if let Some(output_tokens) = usage.get("output_tokens").and_then(|v| v.as_u64())
                    {
                        result.output_tokens = output_tokens as u32;
                    }

                    return Ok(result);
                }
            }
        }
    }

    // Return default if not found
    Ok(Usage::default())
}

/// Parse content blocks from a session
fn build_content_blocks(session: &RecordingSession) -> Result<Vec<ContentBlock>> {
    use crate::llm::ContentBlock;
    use std::collections::HashMap;

    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut current_texts: HashMap<usize, String> = HashMap::new();
    let mut current_block_types: HashMap<usize, String> = HashMap::new();
    let mut tool_properties: HashMap<usize, (String, String)> = HashMap::new(); // (id, name)

    for chunk in &session.chunks {
        if let Ok(json) = serde_json::from_str::<Value>(&chunk.data) {
            let event_type = json.get("type").and_then(Value::as_str);

            match event_type {
                // Handle content block start
                Some("content_block_start") => {
                    if let Some(index) = json.get("index").and_then(|v| v.as_u64()) {
                        let index = index as usize;
                        let content_block = json.get("content_block");

                        if let Some(content_block) = content_block {
                            let block_type = content_block
                                .get("type")
                                .and_then(Value::as_str)
                                .unwrap_or("text");
                            current_block_types.insert(index, block_type.to_string());

                            match block_type {
                                "text" => {
                                    // Initialize text content
                                    current_texts.insert(index, String::new());
                                    let text = content_block
                                        .get("text")
                                        .and_then(Value::as_str)
                                        .unwrap_or("");
                                    current_texts.entry(index).and_modify(|e| e.push_str(text));
                                }
                                "thinking" => {
                                    // Initialize thinking content
                                    current_texts.insert(index, String::new());
                                    let thinking = content_block
                                        .get("thinking")
                                        .and_then(Value::as_str)
                                        .unwrap_or("");
                                    current_texts
                                        .entry(index)
                                        .and_modify(|e| e.push_str(thinking));
                                }
                                "tool_use" => {
                                    // Initialize tool content
                                    current_texts.insert(index, String::new());
                                    let id = content_block
                                        .get("id")
                                        .and_then(Value::as_str)
                                        .unwrap_or("tool-id")
                                        .to_string();
                                    let name = content_block
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("tool-name")
                                        .to_string();
                                    tool_properties.insert(index, (id, name));

                                    if let Some(input) =
                                        content_block.get("input").and_then(Value::as_str)
                                    {
                                        current_texts
                                            .entry(index)
                                            .and_modify(|e| e.push_str(input));
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                // Handle content block delta
                Some("content_block_delta") => {
                    if let Some(index) = json.get("index").and_then(|v| v.as_u64()) {
                        let index = index as usize;

                        if let Some(delta) = json.get("delta") {
                            let delta_type = delta.get("type").and_then(Value::as_str);

                            match delta_type {
                                Some("text_delta") => {
                                    if let Some(text) = delta.get("text").and_then(Value::as_str) {
                                        current_texts
                                            .entry(index)
                                            .or_insert_with(String::new)
                                            .push_str(text);
                                    }
                                }
                                Some("thinking_delta") => {
                                    if let Some(thinking) =
                                        delta.get("thinking").and_then(Value::as_str)
                                    {
                                        current_texts
                                            .entry(index)
                                            .or_insert_with(String::new)
                                            .push_str(thinking);
                                    }
                                }
                                Some("input_json_delta") => {
                                    if let Some(json_part) =
                                        delta.get("partial_json").and_then(Value::as_str)
                                    {
                                        current_texts
                                            .entry(index)
                                            .or_insert_with(String::new)
                                            .push_str(json_part);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Now create all blocks in order
    for index in 0..current_texts.len() {
        if let Some(block_type) = current_block_types.get(&index) {
            match block_type.as_str() {
                "text" => {
                    if let Some(text) = current_texts.get(&index) {
                        blocks.push(ContentBlock::Text { text: text.clone() });
                    }
                }
                "thinking" => {
                    if let Some(thinking) = current_texts.get(&index) {
                        blocks.push(ContentBlock::Thinking {
                            thinking: thinking.clone(),
                            signature: String::new(),
                        });
                    }
                }
                "tool_use" => {
                    if let Some(text) = current_texts.get(&index) {
                        if let Some((id, name)) = tool_properties.get(&index) {
                            let input = if text.is_empty() {
                                serde_json::Value::Null
                            } else {
                                serde_json::from_str(text).unwrap_or(serde_json::Value::Null)
                            };

                            blocks.push(ContentBlock::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input,
                            });
                        }
                    }
                }
                _ => {
                    // Unknown block type, add as plain text
                    if let Some(text) = current_texts.get(&index) {
                        blocks.push(ContentBlock::Text { text: text.clone() });
                    }
                }
            }
        }
    }

    // If no blocks were created, add a default one
    if blocks.is_empty() {
        blocks.push(ContentBlock::Text {
            text: "No content was found in the recorded session".to_string(),
        });
    }

    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

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
