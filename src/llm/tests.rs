use super::*;
use crate::types::ToolDefinition;
use crate::{AnthropicClient, LLMProvider, OpenAIClient};
use anyhow::Result;
use axum::extract::Path;
use axum::{response::IntoResponse, routing::post, Router};
use bytes::Bytes;
use chrono::Utc;
use futures::stream;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::net::TcpListener;

// Test scenario definition
#[derive(Clone)]
struct TestCase {
    name: String,
    request: LLMRequest,
    expected_chunks: Vec<String>,
    expected_response: LLMResponse,
}

impl TestCase {
    fn text_only() -> Self {
        Self {
            name: "Simple text response".to_string(),
            request: LLMRequest {
                messages: vec![Message {
                    role: MessageRole::User,
                    content: MessageContent::Text("Hello".to_string()),
                }],
                system_prompt: "You are a helpful assistant.".to_string(),
                tools: None,
            },
            expected_chunks: vec!["Hi!".to_string(), " How can I help you today?".to_string()],
            expected_response: LLMResponse {
                content: vec![ContentBlock::Text {
                    text: "Hi! How can I help you today?".to_string(),
                }],
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 8,
                },
            },
        }
    }

    fn with_tool() -> Self {
        Self {
            name: "Function calling response".to_string(),
            request: LLMRequest {
                messages: vec![Message {
                    role: MessageRole::User,
                    content: MessageContent::Text("What's the weather?".to_string()),
                }],
                system_prompt: "Use the weather tool.".to_string(),
                tools: Some(vec![ToolDefinition {
                    name: "get_weather".to_string(),
                    description: "Get current weather".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "location": {
                                "type": "string",
                                "description": "Location"
                            }
                        },
                        "required": ["location"]
                    }),
                }]),
            },
            expected_chunks: vec![],
            expected_response: LLMResponse {
                content: vec![ContentBlock::ToolUse {
                    id: "tool-0".to_string(),
                    name: "get_weather".to_string(),
                    input: json!({"location": "current"}),
                }],
                usage: Usage {
                    input_tokens: 15,
                    output_tokens: 12,
                },
            },
        }
    }
}

// Chunk collector for streaming tests
#[derive(Clone)]
struct ChunkCollector {
    chunks: Arc<Mutex<Vec<String>>>,
}

impl ChunkCollector {
    fn new() -> Self {
        Self {
            chunks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn callback(&self) -> StreamingCallback {
        let chunks = self.chunks.clone();
        Box::new(move |chunk: &str| {
            chunks.lock().unwrap().push(chunk.to_string());
            Ok(())
        })
    }

    fn get_chunks(&self) -> Vec<String> {
        self.chunks.lock().unwrap().clone()
    }
}

// Response generator trait for provider-specific implementations
trait MockResponseGenerator: Send + Sync {
    // Generates complete response for non-streaming case
    fn generate_response(&self, case: &TestCase) -> String;
    // Generates chunks for streaming case
    fn generate_chunks(&self, case: &TestCase) -> Vec<Vec<u8>>;
}

// OpenAI implementation
#[derive(Clone)]
struct OpenAIMockGenerator;

impl MockResponseGenerator for OpenAIMockGenerator {
    fn generate_response(&self, case: &TestCase) -> String {
        match case.request.tools {
            None => json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "Hi! How can I help you today?"
                    }
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 8,
                    "total_tokens": 18
                }
            }),
            Some(_) => json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "tool-0",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": "{\"location\":\"current\"}"
                            }
                        }]
                    }
                }],
                "usage": {
                    "prompt_tokens": 15,
                    "completion_tokens": 12,
                    "total_tokens": 27
                }
            }),
        }
        .to_string()
    }

    fn generate_chunks(&self, case: &TestCase) -> Vec<Vec<u8>> {
        match case.request.tools {
            None => vec![
                // Initial content
                b"data: {\"choices\":[{\"delta\":{\"content\":\"Hi!\"},\"finish_reason\":null}]}\n\n".to_vec(),
                // More content
                b"data: {\"choices\":[{\"delta\":{\"content\":\" How can I help you today?\"},\"finish_reason\":null}]}\n\n".to_vec(),
                // Final message with usage
                b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":8,\"total_tokens\":18}}\n\n".to_vec(),
                b"data: [DONE]\n\n".to_vec(),
            ],
            Some(_) => vec![
                // Initial delta with function declaration
                format!(
                    "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":null,\"tool_calls\":[{{\"index\":0,\"id\":\"tool-0\",\"type\":\"function\",\"function\":{{\"name\":\"get_weather\",\"arguments\":\"\"}}}}]}}}}]}}\n\n"
                ).into_bytes(),
                // Arguments streaming in chunks
                format!(
                    "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"function\":{{\"arguments\":\"{{\\\"\"}}}}]}}}}]}}\n\n"
                ).into_bytes(),
                format!(
                    "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"function\":{{\"arguments\":\"location\\\"\"}}}}]}}}}]}}\n\n"
                ).into_bytes(),
                format!(
                    "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"function\":{{\"arguments\":\":\\\"\"}}}}]}}}}]}}\n\n"
                ).into_bytes(),
                format!(
                    "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"function\":{{\"arguments\":\"current\"}}}}]}}}}]}}\n\n"
                ).into_bytes(),
                format!(
                    "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"function\":{{\"arguments\":\"\\\"}}\"}}}}]}}}}]}}\n\n"
                ).into_bytes(),
                // Empty delta with finish reason
                format!(
                    "data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\n"
                ).into_bytes(),
                b"data: [DONE]\n\n".to_vec(),
            ],
        }
    }
}

// Anthropic implementation
#[derive(Clone)]
struct AnthropicMockGenerator;

impl MockResponseGenerator for AnthropicMockGenerator {
    fn generate_response(&self, case: &TestCase) -> String {
        match case.request.tools {
            None => json!({
                "content": [{
                    "type": "text",
                    "text": "Hi! How can I help you today?"
                }],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 8
                }
            }),
            Some(_) => json!({
                "content": [{
                    "type": "tool_use",
                    "id": "tool-0",
                    "name": "get_weather",
                    "input": {"location": "current"}
                }],
                "usage": {
                    "input_tokens": 15,
                    "output_tokens": 12
                }
            }),
        }
        .to_string()
    }

    fn generate_chunks(&self, case: &TestCase) -> Vec<Vec<u8>> {
        match case.request.tools {
            None => vec![
                b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-3\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":8}}}\n\n".to_vec(),
                b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n".to_vec(),
                b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi!\"}}\n\n".to_vec(),
                b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" How can I help you today?\"}}\n\n".to_vec(),
                b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n".to_vec(),
                b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":8}}\n\n".to_vec(),
                b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_vec(),
            ],
            Some(_) => vec![
                b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-3\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":15,\"output_tokens\":2}}}\n\n".to_vec(),
                b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool-0\",\"name\":\"get_weather\"}}\n\n".to_vec(),
                b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"location\\\":\"}}\n\n".to_vec(),
                b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"current\\\"}\"}}\n\n".to_vec(),
                b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n".to_vec(),
                b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":12}}\n\n".to_vec(),
                b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_vec(),
            ],
        }
    }
}

// Vertex implementation
#[derive(Clone)]
struct VertexMockGenerator;

impl MockResponseGenerator for VertexMockGenerator {
    fn generate_response(&self, case: &TestCase) -> String {
        match case.request.tools {
            None => json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "text": "Hi! How can I help you today?"
                        }],
                        "role": "model"
                    }
                }],
                "usageMetadata": {
                    "promptTokenCount": 10,
                    "candidatesTokenCount": 8,
                    "totalTokenCount": 18
                }
            }),
            Some(_) => json!({
                "candidates": [{
                    "content": {
                        "parts": [{
                            "functionCall": {
                                "name": "get_weather",
                                "args": {"location": "current"}
                            }
                        }],
                        "role": "model"
                    }
                }],
                "usageMetadata": {
                    "promptTokenCount": 15,
                    "candidatesTokenCount": 12,
                    "totalTokenCount": 27
                }
            }),
        }
        .to_string()
    }

    fn generate_chunks(&self, case: &TestCase) -> Vec<Vec<u8>> {
        match case.request.tools {
            None => vec![
                format!(
                    "data: {}\n\n",
                    json!({
                        "candidates": [{
                            "content": {
                                "parts": [{"text": "Hi!"}],
                                "role": "model"
                            }
                        }]
                    })
                )
                .into_bytes(),
                format!(
                    "data: {}\n\n",
                    json!({
                        "candidates": [{
                            "content": {
                                "parts": [{"text": " How can I help you today?"}],
                                "role": "model"
                            }
                        }],
                        "usageMetadata": {
                            "promptTokenCount": 10,
                            "candidatesTokenCount": 8,
                            "totalTokenCount": 18
                        }
                    })
                )
                .into_bytes(),
            ],
            Some(_) => vec![format!(
                "data: {}\n\n",
                json!({
                    "candidates": [{
                        "content": {
                            "parts": [{
                                "functionCall": {
                                    "name": "get_weather",
                                    "args": {"location": "current"}
                                }
                            }],
                            "role": "model"
                        }
                    }],
                    "usageMetadata": {
                        "promptTokenCount": 15,
                        "candidatesTokenCount": 12,
                        "totalTokenCount": 27
                    }
                })
            )
            .into_bytes()],
        }
    }
}

// Ollama implementation
#[derive(Clone)]
struct OllamaMockGenerator;

impl MockResponseGenerator for OllamaMockGenerator {
    fn generate_response(&self, case: &TestCase) -> String {
        match case.request.tools {
            None => json!({
                "message": {
                    "content": "Hi! How can I help you today?"
                },
                "done": true,
                "prompt_eval_count": 10,
                "eval_count": 8
            }),
            Some(_) => json!({
                "message": {
                    "content": "",
                    "tool_calls": [{
                        "function": {
                            "name": "get_weather",
                            "arguments": { "location": "current" }
                        }
                    }]
                },
                "done": true,
                "prompt_eval_count": 15,
                "eval_count": 12
            }),
        }
        .to_string()
    }

    fn generate_chunks(&self, case: &TestCase) -> Vec<Vec<u8>> {
        match case.request.tools {
            None => vec![
                format!(
                    "{}\n",
                    json!({
                        "message": {
                            "content": "Hi!"
                        },
                        "done": false,
                        "prompt_eval_count": 10,
                        "eval_count": 4
                    })
                )
                .into_bytes(),
                format!(
                    "{}\n",
                    json!({
                        "message": {
                            "content": " How can I help you today?"
                        },
                        "done": true,
                        "prompt_eval_count": 10,
                        "eval_count": 8
                    })
                )
                .into_bytes(),
            ],
            Some(_) => vec![format!(
                "{}\n",
                json!({
                    "message": {
                        "content": "",
                        "tool_calls": [{
                            "function": {
                                "name": "get_weather",
                                "arguments": { "location": "current" }
                            }
                        }]
                    },
                    "done": true,
                    "prompt_eval_count": 15,
                    "eval_count": 12
                })
            )
            .into_bytes()],
        }
    }
}

// Helper to create a mock server
async fn create_mock_server(
    test_case: TestCase,
    generator: impl MockResponseGenerator + Clone + 'static,
) -> String {
    let app = Router::new().route(
        "/*path",
        post(
            move |Path(path): Path<String>, req: axum::extract::Json<serde_json::Value>| {
                let generator = generator.clone();
                let test_case = test_case.clone();
                async move {
                    let is_streaming = path.contains("stream")
                        || req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

                    if is_streaming {
                        let chunks = generator.generate_chunks(&test_case);
                        let stream = stream::iter(
                            chunks
                                .into_iter()
                                .map(|chunk| Ok::<_, std::io::Error>(Bytes::from(chunk))),
                        );

                        axum::response::Response::builder()
                            .status(axum::http::StatusCode::OK)
                            .header("content-type", "text/event-stream")
                            .body(axum::body::Body::from_stream(stream))
                            .unwrap()
                    } else {
                        (
                            axum::http::StatusCode::OK,
                            axum::Json(
                                serde_json::from_str::<serde_json::Value>(
                                    &generator.generate_response(&test_case),
                                )
                                .unwrap(),
                            ),
                        )
                            .into_response()
                    }
                }
            },
        ),
    );

    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = TcpListener::bind(addr).await.unwrap();
    let server_addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    format!("http://{}", server_addr)
}

// Helper to create a rate-limited mock server
async fn create_rate_limited_mock_server(
    attempts_until_success: usize,
    error_response: serde_json::Value,
    rate_limit_headers: std::collections::HashMap<String, String>,
) -> String {
    let attempts = Arc::new(Mutex::new(0));

    let app = Router::new().route(
        "/*path",
        post(move |_req: axum::extract::Json<serde_json::Value>| {
            let attempts = attempts.clone();
            let error_response = error_response.clone();
            let rate_limit_headers = rate_limit_headers.clone();
            async move {
                let mut current_attempts = attempts.lock().unwrap();
                *current_attempts += 1;

                if *current_attempts > attempts_until_success {
                    // After specified attempts, return success
                    (
                        axum::http::StatusCode::OK,
                        axum::Json(json!({
                            "content": [{
                                "type": "text",
                                "text": "Success after retry!"
                            }],
                            "usage": {
                                "input_tokens": 10,
                                "output_tokens": 8
                            }
                        })),
                    )
                        .into_response()
                } else {
                    // Return rate limit error with headers
                    let mut response = axum::response::Response::builder()
                        .status(axum::http::StatusCode::TOO_MANY_REQUESTS);

                    // Add rate limit headers
                    for (key, value) in rate_limit_headers.iter() {
                        response = response.header(key, value);
                    }

                    response
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(
                            serde_json::to_string(&error_response).unwrap(),
                        ))
                        .unwrap()
                }
            }
        }),
    );

    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = TcpListener::bind(addr).await.unwrap();
    let server_addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    format!("http://{}", server_addr)
}

// Run all test cases for a given provider configuration
async fn run_provider_tests<T: MockResponseGenerator + Clone + 'static>(
    provider_name: &str,
    create_client: impl Fn(&str) -> Box<dyn LLMProvider>,
    generator: T,
) -> Result<()> {
    let test_cases = vec![TestCase::text_only(), TestCase::with_tool()];

    for case in test_cases {
        println!("Running {} test case: {}", provider_name, case.name);

        let base_url = create_mock_server(case.clone(), generator.clone()).await;
        let client = create_client(&base_url);

        // Test non-streaming
        let response = client.send_message(case.request.clone(), None).await?;

        assert_eq!(
            response.content, case.expected_response.content,
            "Non-streaming content mismatch"
        );
        assert_eq!(
            response.usage, case.expected_response.usage,
            "Non-streaming usage mismatch"
        );

        // Test streaming
        let collector = ChunkCollector::new();
        let callback = collector.callback();

        let response = client
            .send_message(case.request.clone(), Some(&callback))
            .await?;

        assert_eq!(
            response.content, case.expected_response.content,
            "Streaming content mismatch"
        );
        assert_eq!(
            collector.get_chunks(),
            case.expected_chunks,
            "Streaming chunks mismatch"
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_openai_provider() -> Result<()> {
    run_provider_tests(
        "OpenAI",
        |url| {
            Box::new(OpenAIClient::new_with_base_url(
                "test-key".to_string(),
                "gpt-4".to_string(),
                url.to_string(),
            ))
        },
        OpenAIMockGenerator,
    )
    .await
}

#[tokio::test]
async fn test_anthropic_provider() -> Result<()> {
    run_provider_tests(
        "Anthropic",
        |url| {
            Box::new(AnthropicClient::new_with_base_url(
                "test-key".to_string(),
                "claude-3".to_string(),
                url.to_string(),
            ))
        },
        AnthropicMockGenerator,
    )
    .await
}

#[tokio::test]
async fn test_vertex_provider() -> Result<()> {
    run_provider_tests(
        "Vertex",
        |url| {
            Box::new(VertexClient::new_with_base_url(
                "test-key".to_string(),
                "gemini-pro".to_string(),
                url.to_string(),
            ))
        },
        VertexMockGenerator,
    )
    .await
}

#[tokio::test]
async fn test_ollama_provider() -> Result<()> {
    run_provider_tests(
        "Ollama",
        |url| {
            Box::new(OllamaClient::new_with_base_url(
                "llama2".to_string(),
                4096,
                url.to_string(),
            ))
        },
        OllamaMockGenerator,
    )
    .await
}

#[tokio::test]
async fn test_anthropic_rate_limit_retry() -> Result<()> {
    // Configure rate limit error response
    let error_response = json!({
        "type": "error",
        "error": {
            "type": "rate_limit_error",
            "message": "Rate limit exceeded. Please retry after 5 seconds."
        }
    });

    // Configure rate limit headers
    let mut headers = std::collections::HashMap::new();
    headers.insert(
        "anthropic-ratelimit-requests-reset".to_string(),
        (Utc::now() + chrono::Duration::seconds(1)).to_rfc3339(),
    );
    headers.insert("retry-after".to_string(), "1".to_string());

    // Create a mock server that will fail with rate limit errors 3 times before succeeding
    let base_url = create_rate_limited_mock_server(2, error_response, headers).await;

    // Create client with fast retry timings for test
    let client = AnthropicClient::new_with_base_url(
        "test-key".to_string(),
        "claude-3".to_string(),
        base_url,
    );

    // Send a test message that should trigger retries
    let request = LLMRequest {
        messages: vec![Message {
            role: MessageRole::User,
            content: MessageContent::Text("Hello".to_string()),
        }],
        system_prompt: "You are a helpful assistant.".to_string(),
        tools: None,
    };

    // The request should eventually succeed after retries
    let response = client.send_message(request, None).await?;

    // Verify we got the success response
    assert_eq!(
        response.content,
        vec![ContentBlock::Text {
            text: "Success after retry!".to_string()
        }]
    );

    Ok(())
}
