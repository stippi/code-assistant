# StreamingCallback Extension for Status Information

This document outlines the implementation plan for extending the streaming architecture to support status information (LLM request sent, rate limit countdowns, processing indicators, etc.) through the existing StreamingChunk → DisplayFragment pipeline.

## Overview

We will extend the existing `StreamingChunk` enum with status variants that flow through the current streaming architecture, providing elegant status updates to UI implementations without breaking existing functionality.

## Architecture Summary

### Current Flow
```
LLM Providers → StreamingChunk → StreamProcessor → DisplayFragment → UserInterface
```

### Enhanced Flow
```
LLM Providers → StreamingChunk (including Status) → StreamProcessor → DisplayFragment (including Status) → UserInterface
```

## Implementation Plan

### Phase 1: Core Type Extensions

#### 1.1 Extend StreamingChunk in LLM crate
**File:** `crates/llm/src/types.rs`

Add new status-related variants to the `StreamingChunk` enum:

```rust
/// Structure to represent different types of streaming content from LLMs
#[derive(Debug, Clone)]
pub enum StreamingChunk {
    /// Regular text content
    Text(String),
    /// Content identified as "thinking" (supported by some models)
    Thinking(String),
    /// JSON input for tool calls with optional metadata
    InputJson {
        content: String,
        tool_name: Option<String>,
        tool_id: Option<String>,
    },
    /// Status information about the LLM request/response cycle
    Status(StatusInfo),
}

#[derive(Debug, Clone)]
pub enum StatusInfo {
    /// LLM request has been sent and we're waiting for response
    RequestSent {
        provider: String,
        model: Option<String>,
        timestamp: std::time::SystemTime,
    },
    /// Rate limit encountered, showing countdown
    RateLimitWait {
        provider: String,
        remaining_seconds: u64,
    },
    /// Request is being processed by the LLM
    RequestCompleted {
        provider: String,
        timestamp: std::time::SystemTime,
    },
    /// Authentication/connection issues
    ConnectionIssue {
        provider: String,
        error_type: String,
        retry_attempt: u32,
        max_attempts: u32,
    },
    /// Custom status message
    Message {
        message: String,
        level: StatusLevel,
        persistent: bool, // Whether this should stay visible
    },
}

#[derive(Debug, Clone)]
pub enum StatusLevel {
    Info,
    Warning,
    Error,
}
```

#### 1.2 Extend DisplayFragment in UI module
**File:** `crates/code_assistant/src/ui/streaming/mod.rs`

Add status variants to the `DisplayFragment` enum:

```rust
/// Fragments for display in UI components
#[derive(Debug, Clone)]
pub enum DisplayFragment {
    /// Regular plain text
    PlainText(String),
    /// Thinking text (shown differently)
    ThinkingText(String),
    /// Tool invocation start
    ToolName { name: String, id: String },
    /// Parameter for a tool
    ToolParameter {
        name: String,
        value: String,
        tool_id: String,
    },
    /// End of a tool invocation
    ToolEnd { id: String },
    /// Status information for display (reuses StatusInfo from llm crate)
    Status(StatusInfo),
}
```

Note: We directly reuse `StatusInfo` and `StatusLevel` from the LLM crate to avoid duplication. The UI module should import these types:

```rust
use llm::{StatusInfo, StatusLevel};
```

### Phase 2: Stream Processor Updates

#### 2.1 Update JsonStreamProcessor
**File:** `crates/code_assistant/src/ui/streaming/json_processor.rs`

Extend the `process` method to handle `StreamingChunk::Status`:

```rust
fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError> {
    match chunk {
        // ... existing cases ...

        // Handle status chunks - direct pass-through, no conversion needed
        StreamingChunk::Status(status_info) => {
            self.ui.display_fragment(&DisplayFragment::Status(status_info.clone()))
        }
    }
}
```

The stream processor now simply passes through the status information without any transformation, eliminating unnecessary complexity.

#### 2.2 Update XmlStreamProcessor
**File:** `crates/code_assistant/src/ui/streaming/xml_processor.rs`

Apply similar changes to the XML processor's `process` method:

```rust
fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError> {
    match chunk {
        // ... existing cases ...

        // Handle status chunks - direct pass-through, no conversion needed
        StreamingChunk::Status(status_info) => {
            self.ui.display_fragment(&DisplayFragment::Status(status_info.clone()))
        }
    }
}
```

### Phase 3: UI Implementation Updates

#### 3.1 Update GPUI UserInterface
**File:** `crates/code_assistant/src/ui/gpui/mod.rs`

Extend the `display_fragment` method to handle status fragments:

```rust
fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
    match fragment {
        // ... existing cases ...

        DisplayFragment::Status(status_info) => {
            self.push_event(UiEvent::StatusUpdate {
                status_info: status_info.clone(),
            });
        }
    }
    Ok(())
}
```

#### 3.2 Add Status UiEvent
**File:** `crates/code_assistant/src/ui/gpui/ui_events.rs`

Add a new event type for status updates:

```rust
#[derive(Clone, Debug)]
pub enum UiEvent {
    // ... existing variants ...

    StatusUpdate {
        status_info: StatusInfo,
    },
}
```

#### 3.3 Status Display Components
**File:** `crates/code_assistant/src/ui/gpui/elements/status_bar.rs` (new file)

Create a status bar component to display status information:

```rust
use gpui::*;
use llm::StatusInfo;

pub struct StatusBar {
    current_status: Option<StatusInfo>,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            current_status: None,
        }
    }

    pub fn update_status(&mut self, status: StatusInfo) {
        self.current_status = Some(status);
    }
}

impl Render for StatusBar {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        // Render status information with appropriate styling
        // Include countdown timers, progress indicators, etc.
    }
}
```

### Phase 4: LLM Provider Integration

#### 4.1 Update Anthropic Provider
**File:** `crates/llm/src/anthropic.rs`

Add status emissions at key points in the request lifecycle:

```rust
impl AnthropicClient {
    async fn try_send_request(
        &self,
        request: &AnthropicRequest,
        streaming_callback: Option<&StreamingCallback>,
    ) -> Result<(LLMResponse, AnthropicRateLimitInfo)> {

        // Emit request started status
        if let Some(callback) = streaming_callback {
            callback(&StreamingChunk::Status(StatusInfo::RequestSent {
                provider: "anthropic".to_string(),
                model: Some(self.model.clone()),
                timestamp: std::time::SystemTime::now(),
            }))?;
        }

        // ... existing implementation ...

        // On rate limit
        if let Err(ApiErrorContext { error: ApiError::RateLimit(_), rate_limits: Some(limits) }) = result {
            if let Some(callback) = streaming_callback {
                let delay = limits.get_retry_delay();
                callback(&StreamingChunk::Status(StatusInfo::RateLimitWait {
                    provider: "anthropic".to_string(),
                    remaining_seconds: delay.as_secs(),
                }))?;
            }
        }
    }
}
```

#### 4.2 Update Other Providers
Apply similar patterns to:
- **File:** `crates/llm/src/openai.rs`
- **File:** `crates/llm/src/vertex.rs`
- **File:** `crates/llm/src/ollama.rs`
- **File:** `crates/llm/src/aicore_invoke.rs`

### Phase 5: Agent Runner Integration

#### 5.1 Enhanced Request Context
**File:** `crates/code_assistant/src/agent/runner.rs`

Update the `get_next_assistant_message` method to provide richer context:

```rust
impl Agent {
    async fn get_next_assistant_message(&self, messages: Vec<Message>) -> Result<llm::LLMResponse> {
        let request_id = self.ui.begin_llm_request().await?;

        // Create enhanced context for status tracking
        let ui = Arc::clone(&self.ui);
        let processor = Arc::new(Mutex::new(create_stream_processor(self.tool_mode, ui.clone())));

        let streaming_callback: StreamingCallback = Box::new(move |chunk: &StreamingChunk| {
            // Handle status chunks with additional context
            if let StreamingChunk::Status(status) = chunk {
                // Potentially enhance status with request_id or other context
            }

            let mut processor_guard = processor.lock().unwrap();
            processor_guard
                .process(chunk)
                .map_err(|e| anyhow::anyhow!("Failed to process streaming chunk: {}", e))
        });

        // ... rest of implementation
    }
}
```

### Phase 6: Testing and Documentation

#### 6.1 Unit Tests
**Files:**
- `crates/llm/src/tests.rs` - Test status chunk generation
- `crates/code_assistant/src/ui/streaming/json_processor_tests.rs` - Test status processing
- `crates/code_assistant/src/ui/streaming/xml_processor_tests.rs` - Test status processing

#### 6.2 Integration Tests
**File:** `crates/code_assistant/src/tests/integration_tests.rs` (new file)

Test end-to-end status flow from LLM providers to UI display.

#### 6.3 Documentation Updates
**Files:**
- Update existing documentation in module headers
- Add examples of status usage in doc comments

## Implementation Timeline

1. **Week 1**: Phase 1 & 2 - Core type extensions and stream processor updates
2. **Week 2**: Phase 3 - UI implementation updates and status display components
3. **Week 3**: Phase 4 - LLM provider integration (start with Anthropic)
4. **Week 4**: Phase 5 & 6 - Agent runner integration, testing, and documentation

## Benefits

1. **Architectural Consistency**: Follows existing patterns and data flow
2. **Non-Breaking**: Existing code continues to work unchanged
3. **Extensible**: Easy to add new status types
4. **Type-Safe**: Leverages Rust's enum system
5. **Thread-Safe**: Uses existing synchronization patterns
6. **Testable**: Each component can be unit tested independently
7. **No Duplication**: Single source of truth for status types, avoiding maintenance overhead
8. **Simplified Flow**: Stream processors perform direct pass-through for status, eliminating unnecessary transformations

## Future Enhancements

- **Real-time Countdown**: UI components can update countdown timers
- **Status History**: Maintain a log of status events
- **Configurable Display**: User preferences for status visibility
- **Status Aggregation**: Combine multiple status sources
- **Metrics Collection**: Gather statistics on request patterns and timing

This implementation plan provides a solid foundation for status information while maintaining the elegant architecture already established in the codebase.
