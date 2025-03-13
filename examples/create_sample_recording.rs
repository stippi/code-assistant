//! Example script to create a sample recording file
//!
//! This script creates a recording file with a simple conversation
//! that can be used for testing the RecordingPlayer.

use std::fs::File;
use std::io::Write;

/// Example recorded chunk
#[derive(serde::Serialize)]
struct RecordedChunk {
    data: String,
    timestamp_ms: u64,
}

/// Example recording session
#[derive(serde::Serialize)]
struct RecordingSession {
    request: serde_json::Value,
    timestamp: chrono::DateTime<chrono::Utc>,
    chunks: Vec<RecordedChunk>,
}

fn main() {
    // Create a sample recording session with Anthropic-style events
    let session = RecordingSession {
        request: serde_json::json!({
            "messages": [
                {"role": "user", "content": "What is Rust?"}
            ],
            "system": "You are a helpful assistant.",
            "model": "claude-3-7-sonnet-20250219",
            "max_tokens": 1024
        }),
        timestamp: chrono::Utc::now(),
        chunks: vec![
            // Message start event
            RecordedChunk {
                data: r#"{"type":"message_start","message":{"id":"msg_test1","type":"message","role":"assistant","model":"claude-3-7-sonnet-20250219","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#.to_string(),
                timestamp_ms: 0,
            },
            // Content block start event
            RecordedChunk {
                data: r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#.to_string(),
                timestamp_ms: 100,
            },
            // Text deltas
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Rust"}}"#.to_string(),
                timestamp_ms: 150,
            },
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" is"}}"#.to_string(),
                timestamp_ms: 200,
            },
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" a"}}"#.to_string(),
                timestamp_ms: 250,
            },
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" systems"}}"#.to_string(),
                timestamp_ms: 300,
            },
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" programming"}}"#.to_string(),
                timestamp_ms: 350,
            },
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" language"}}"#.to_string(),
                timestamp_ms: 400,
            },
            // Add thinking section
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"\n\n<thinking>"}}"#.to_string(),
                timestamp_ms: 500,
            },
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"I should mention memory safety here."}}"#.to_string(),
                timestamp_ms: 550,
            },
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"</thinking>\n\n"}}"#.to_string(),
                timestamp_ms: 600,
            },
            // More content
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"One of Rust's key features is its focus on memory safety without sacrificing performance."}}"#.to_string(),
                timestamp_ms: 700,
            },
            // Tool usage example
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"\n\nLet me show you an example of Rust code:"}}"#.to_string(),
                timestamp_ms: 800,
            },
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"\n\n<tool:write_file>"}}"#.to_string(),
                timestamp_ms: 900,
            },
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"\n<param:path>example.rs</param:path>"}}"#.to_string(),
                timestamp_ms: 950,
            },
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"\n<param:content>fn main() {\n    println!(\"Hello, world!\");\n}</param:content>"}}"#.to_string(),
                timestamp_ms: 1000,
            },
            RecordedChunk {
                data: r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"\n</tool:write_file>"}}"#.to_string(),
                timestamp_ms: 1050,
            },
            // Final message deltas and content block stop
            RecordedChunk {
                data: r#"{"type":"content_block_stop","index":0}"#.to_string(),
                timestamp_ms: 1100,
            },
            RecordedChunk {
                data: r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":120}}"#.to_string(),
                timestamp_ms: 1150,
            },
            RecordedChunk {
                data: r#"{"type":"message_stop"}"#.to_string(),
                timestamp_ms: 1200,
            },
        ],
    };

    // Create an array with one recording session
    let recordings = vec![session];

    // Write to a file
    let file_path = "sample_recording.json";
    let mut file = File::create(file_path).expect("Failed to create file");
    file.write_all(serde_json::to_string_pretty(&recordings).unwrap().as_bytes())
        .expect("Failed to write to file");

    println!("Sample recording file created: {}", file_path);
}
