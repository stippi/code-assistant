use crate::tests::utils::parse_and_truncate_llm_response;
use anyhow::Result;
use axum::{routing::post, Router};
use llm::types::*;
use llm::{AnthropicClient, LLMProvider, StreamingCallback, StreamingChunk};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

#[tokio::test]
async fn test_tool_limit_with_realistic_anthropic_chunks() -> Result<()> {
    // These are the exact chunks from the user's log that show the truncation issue
    let chunks = vec![
        "I'll help",
        " you ref",
        "actor the A",
        "nthropic client:",
        "\n\n<tool",
        ":",
        "rea",
        "d_",
        "files",
        ">\n<param",
        ":",
        "project",
        ">",
        "code-assistant",
        "</param:project>",
        "\n<param:",
        "path",
        ">",
        "crates/ll",
        "m/src/",
        "anthropic.rs",
        "</param:path",
        ">\n</tool",
        ":read_files",
        ">\n\n---", // Extra content that should be truncated
    ];

    // Create a mock Anthropic server that returns these exact chunks
    let app = Router::new().route(
        "/messages",
        post({
            let chunks = chunks.clone();
            move |_req: axum::extract::Request| async move {
                let mut sse_response = String::new();

                // Start message event
                sse_response.push_str("data: ");
                sse_response.push_str(
                    &serde_json::to_string(&json!({
                        "type": "message_start",
                        "message": {
                            "id": "msg_test",
                            "type": "message",
                            "role": "assistant",
                            "model": "claude-3",
                            "usage": {
                                "input_tokens": 10,
                                "output_tokens": 0,
                                "cache_creation_input_tokens": 0,
                                "cache_read_input_tokens": 0
                            }
                        }
                    }))
                    .unwrap(),
                );
                sse_response.push_str("\n\n");

                // Content block start
                sse_response.push_str("data: ");
                sse_response.push_str(
                    &serde_json::to_string(&json!({
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {
                            "type": "text",
                            "text": ""
                        }
                    }))
                    .unwrap(),
                );
                sse_response.push_str("\n\n");

                // Send each chunk as a delta
                for chunk in chunks {
                    sse_response.push_str("data: ");
                    sse_response.push_str(
                        &serde_json::to_string(&json!({
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {
                                "type": "text_delta",
                                "text": chunk
                            }
                        }))
                        .unwrap(),
                    );
                    sse_response.push_str("\n\n");
                }

                // Content block stop
                sse_response.push_str("data: ");
                sse_response.push_str(
                    &serde_json::to_string(&json!({
                        "type": "content_block_stop",
                        "index": 0
                    }))
                    .unwrap(),
                );
                sse_response.push_str("\n\n");

                // Message stop
                sse_response.push_str("data: ");
                sse_response.push_str(
                    &serde_json::to_string(&json!({
                        "type": "message_stop"
                    }))
                    .unwrap(),
                );
                sse_response.push_str("\n\n");

                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .header("cache-control", "no-cache")
                    .body(sse_response)
                    .unwrap()
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{addr}");

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give the server time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Create the Anthropic client
    let mut anthropic_client =
        AnthropicClient::new("test-key".to_string(), "claude-3".to_string(), base_url);

    // Track whether tool limit was triggered and accumulate text
    let tool_limit_triggered = Arc::new(Mutex::new(false));
    let tool_limit_triggered_clone = tool_limit_triggered.clone();
    let accumulated_text = Arc::new(Mutex::new(String::new()));
    let accumulated_text_clone = accumulated_text.clone();

    // Simple callback that triggers tool limit after seeing complete tool end
    let callback: StreamingCallback = Box::new(move |chunk: &StreamingChunk| -> Result<()> {
        match chunk {
            StreamingChunk::Text(text) => {
                // Accumulate text to check for tool completion
                {
                    let mut acc = accumulated_text_clone.lock().unwrap();
                    acc.push_str(text);

                    // Trigger tool limit when we see the complete tool end
                    if acc.contains("</tool:read_files>") {
                        *tool_limit_triggered_clone.lock().unwrap() = true;
                        return Err(anyhow::anyhow!(
                            "Tool limit reached - only one tool per message allowed"
                        ));
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    });

    // Create the request
    let request = LLMRequest {
        messages: vec![Message {
            role: MessageRole::User,
            content: MessageContent::Text("Please refactor the Anthropic client".to_string()),
            ..Default::default()
        }],
        system_prompt: "You are a helpful assistant.".to_string(),
        ..Default::default()
    };

    // Send the request with streaming - this should trigger tool limit detection
    let result = anthropic_client
        .send_message(request, Some(&callback))
        .await;

    // Debug: print what happened
    println!("Result: {result:?}");
    println!(
        "Tool limit triggered: {}",
        *tool_limit_triggered.lock().unwrap()
    );

    assert!(result.is_ok());

    let response = result.unwrap();

    // Verify the response contains the complete text up to and including the tool
    assert_eq!(response.content.len(), 1);
    if let ContentBlock::Text { text, .. } = &response.content[0] {
        println!("Final LLM response text: '{text}'");
        println!("Text length: {}", text.len());

        // Check that we have the complete tool text but not the extra content
        assert!(text.contains("I'll help you refactor the Anthropic client:"));
        assert!(text.contains("<tool:read_files>"));
        assert!(text.contains("<param:project>code-assistant</param:project>"));
        assert!(text.contains("<param:path>crates/llm/src/anthropic.rs</param:path>"));
        assert!(
            text.contains("</tool:read_files>"),
            "Should contain complete tool"
        );

        // Now test the parse_and_truncate_llm_response function from agent runner
        println!("\n🔍 Testing parse_and_truncate_llm_response function:");
        let request_id = 42;

        match parse_and_truncate_llm_response(&response, request_id) {
            Ok((tool_requests, truncated_response)) => {
                println!("✅ parse_and_truncate_llm_response succeeded");
                println!("Tool requests found: {}", tool_requests.len());
                println!(
                    "Truncated response content blocks: {}",
                    truncated_response.content.len()
                );

                // Check that we parsed exactly one tool
                assert_eq!(tool_requests.len(), 1, "Should parse exactly one tool");

                let tool_request = &tool_requests[0];
                println!("Tool name: {}", tool_request.name);
                println!("Tool input: {}", tool_request.input);

                assert_eq!(tool_request.name, "read_files");
                assert_eq!(
                    tool_request.input.get("project").unwrap().as_str().unwrap(),
                    "code-assistant"
                );
                assert_eq!(
                    tool_request.input.get("paths").unwrap().as_array().unwrap()[0],
                    "crates/llm/src/anthropic.rs"
                );

                // Check the truncated response
                if let ContentBlock::Text {
                    text: truncated_text,
                    ..
                } = &truncated_response.content[0]
                {
                    println!("Truncated text: '{truncated_text}'");
                    println!("Truncated text length: {}", truncated_text.len());

                    // The key test: truncated response should end exactly at the tool close tag
                    assert!(
                        truncated_text.ends_with("</tool:read_files>"),
                        "Truncated text should end with complete tool close tag, but ends with: {}",
                        &truncated_text[truncated_text.len().saturating_sub(20)..]
                    );

                    // Should NOT contain the extra content after the tool
                    assert!(
                        !truncated_text.contains("---"),
                        "Truncated text should not contain extra content after tool: {truncated_text}"
                    );
                } else {
                    panic!(
                        "Expected Text content block in truncated response, got: {:?}",
                        truncated_response.content[0]
                    );
                }
            }
            Err(e) => {
                println!("❌ parse_and_truncate_llm_response failed: {e:?}");
                panic!("parse_and_truncate_llm_response should succeed with valid tool response");
            }
        }
    } else {
        panic!(
            "Expected text content block, got: {:?}",
            response.content[0]
        );
    }

    // Verify that tool limit was triggered during processing
    assert!(
        *tool_limit_triggered.lock().unwrap(),
        "Tool limit should have been triggered"
    );

    Ok(())
}
