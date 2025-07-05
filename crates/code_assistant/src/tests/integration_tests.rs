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
        ">\n\n---", // More extra content
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
    let base_url = format!("http://{}", addr);

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
            request_id: None,
            usage: None,
        }],
        system_prompt: "You are a helpful assistant.".to_string(),
        tools: None,
        stop_sequences: None,
    };

    // Send the request with streaming - this should trigger tool limit detection
    let result = anthropic_client
        .send_message(request, Some(&callback))
        .await;

    // Debug: print what happened
    println!("Result: {:?}", result);
    println!(
        "Tool limit triggered: {}",
        *tool_limit_triggered.lock().unwrap()
    );

    // Currently, the request fails completely when tool limit is reached
    // This demonstrates the issue we need to fix: the LLM provider should
    // handle tool limit errors gracefully and return a successful response
    // with the content truncated at the appropriate point

    if result.is_ok() {
        println!("✅ LLM provider handled tool limit gracefully");
        let response = result.unwrap();

        // Verify the response contains the complete text up to and including the tool
        assert_eq!(response.content.len(), 1);
        if let ContentBlock::Text { text } = &response.content[0] {
            println!("Final LLM response text: '{}'", text);
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
        } else {
            panic!(
                "Expected text content block, got: {:?}",
                response.content[0]
            );
        }
    } else {
        println!("❌ LLM provider failed instead of handling tool limit gracefully");
        println!("Error: {:?}", result.unwrap_err());

        // This demonstrates the bug that needs to be fixed:
        // When a tool limit is reached during streaming, the LLM provider should:
        // 1. Stop processing new chunks
        // 2. Finalize the content blocks with accumulated content
        // 3. Return a successful LLMResponse with the content truncated appropriately
        //
        // Currently, it propagates the error and fails the entire request
        println!("This test demonstrates that the LLM provider needs to handle tool limit errors gracefully");
    }

    // Verify that tool limit was triggered during processing
    assert!(
        *tool_limit_triggered.lock().unwrap(),
        "Tool limit should have been triggered"
    );

    Ok(())
}
