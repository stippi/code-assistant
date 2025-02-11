use super::*;
use crate::types::ToolDefinition;
use anyhow::Result;
use mockito::{Matcher, Server};
use serde_json::json;
use std::sync::{Arc, Mutex};

// Test scenario definition
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
            expected_chunks: vec![], // Tool calls typically don't stream text
            expected_response: LLMResponse {
                content: vec![ContentBlock::ToolUse {
                    id: "test-1".to_string(),
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
trait MockResponseGenerator {
    // Asserts that the request matches expectations and returns mocked response
    fn assert_request(&self, is_streaming: bool) -> Matcher;

    // Generates complete response for non-streaming case
    fn generate_response(&self, case: &TestCase) -> String;

    // Generates chunks for streaming case
    fn generate_chunks(&self, case: &TestCase) -> Vec<Vec<u8>>;
}

// OpenAI implementation
struct OpenAIMockGenerator;

impl MockResponseGenerator for OpenAIMockGenerator {
    fn assert_request(&self, is_streaming: bool) -> Matcher {
        Matcher::Json(json!({
            "model": "gpt-4",
            "stream": is_streaming,
            "messages": json!({}), // Just assert presence of messages field
            // We could add more specific assertions here if needed
        }))
    }

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
                            "id": "test-1",
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
                // Tool call start
                b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"test-1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\"}}]},\"finish_reason\":null}]}\n\n".to_vec(),
                // Tool call arguments
                b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"location\\\":\\\"current\\\"\"}}]},\"finish_reason\":null}]}\n\n".to_vec(),
                b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":15,\"completion_tokens\":12,\"total_tokens\":27}}\n\n".to_vec(),
                b"data: [DONE]\n\n".to_vec(),
            ],
        }
    }
}

// Anthropic implementation
struct AnthropicMockGenerator;

impl MockResponseGenerator for AnthropicMockGenerator {
    fn assert_request(&self, is_streaming: bool) -> Matcher {
        Matcher::Json(json!({
            "model": "claude-3",
            "max_tokens": 8192,
            "stream": is_streaming,
            "messages": json!({}), // Just assert presence of messages field
        }))
    }

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
                    "id": "test-1",
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
                b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-3\"}}\n\n".to_vec(),
                b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n".to_vec(),
                b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi!\"}}\n\n".to_vec(),
                b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" How can I help you today?\"}}\n\n".to_vec(),
                b"data: {\"type\":\"content_block_stop\",\"index\":0}\n\n".to_vec(),
                b"data: {\"type\":\"message_stop\"}\n\n".to_vec(),
            ],
            Some(_) => vec![
                b"data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-3\"}}\n\n".to_vec(),
                b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"test-1\",\"name\":\"get_weather\"}}\n\n".to_vec(),
                b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"location\\\":\\\"current\\\"}\"}}\n\n".to_vec(),
                b"data: {\"type\":\"content_block_stop\",\"index\":0}\n\n".to_vec(),
                b"data: {\"type\":\"message_stop\"}\n\n".to_vec(),
            ],
        }
    }
}

// Run all test cases for a given provider configuration
async fn run_provider_tests<T: MockResponseGenerator>(
    provider_name: &str,
    create_client: impl Fn(&str) -> Box<dyn LLMProvider>,
    generator: T,
) -> Result<()> {
    let test_cases = vec![TestCase::text_only(), TestCase::with_tool()];

    let mut server = Server::new();

    for case in test_cases {
        println!("Running {} test case: {}", provider_name, case.name);

        // Test non-streaming
        let mock = server
            .mock("POST", "/chat/completions")
            .match_body(generator.assert_request(false))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(generator.generate_response(&case))
            .create();

        let client = create_client(&server.url());
        let response = client.send_message(case.request.clone(), None).await?;

        assert_eq!(
            response.content, case.expected_response.content,
            "Non-streaming content mismatch"
        );
        assert_eq!(
            response.usage, case.expected_response.usage,
            "Non-streaming usage mismatch"
        );

        mock.assert();

        // Test streaming
        let chunks = generator.generate_chunks(&case);
        let mock = server
            .mock("POST", "/chat/completions")
            .match_body(generator.assert_request(true))
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_chunked_body(move |w| {
                for chunk in chunks.iter() {
                    w.write_all(&chunk)?;
                }
                Ok(())
            })
            .create();

        let collector = ChunkCollector::new();
        let callback = collector.callback();

        let response = client.send_message(case.request, Some(&callback)).await?;

        assert_eq!(
            response.content, case.expected_response.content,
            "Streaming content mismatch"
        );
        assert_eq!(
            collector.get_chunks(),
            case.expected_chunks,
            "Streaming chunks mismatch"
        );

        mock.assert();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_openai_provider() -> Result<()> {
        run_provider_tests(
            "OpenAI",
            |url| {
                Box::new(OpenAIClient::new_with_base_url(
                    "test-key".to_string(),
                    "gpt-4".to_string(),
                    format!("{}/chat/completions", url),
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
                    format!("{}/chat/completions", url),
                ))
            },
            AnthropicMockGenerator,
        )
        .await
    }
}
