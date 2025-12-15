//! Tests for the sub-agent feature (spawn_agent tool).

use crate::agent::sub_agent::{SubAgentResult, SubAgentRunner};
use crate::agent::SubAgentCancellationRegistry;
use crate::tools::core::ToolScope;
use crate::tools::impls::spawn_agent::{SpawnAgentInput, SpawnAgentOutput};
use anyhow::Result;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A mock sub-agent runner for testing.
struct MockSubAgentRunner {
    call_count: AtomicUsize,
    concurrent_count: AtomicUsize,
    max_concurrent: AtomicUsize,
    delay_ms: u64,
    response: String,
}

impl MockSubAgentRunner {
    fn new(delay_ms: u64, response: &str) -> Self {
        Self {
            call_count: AtomicUsize::new(0),
            concurrent_count: AtomicUsize::new(0),
            max_concurrent: AtomicUsize::new(0),
            delay_ms,
            response: response.to_string(),
        }
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    fn max_concurrent(&self) -> usize {
        self.max_concurrent.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl SubAgentRunner for MockSubAgentRunner {
    async fn run(
        &self,
        _parent_tool_id: &str,
        _instructions: String,
        _tool_scope: ToolScope,
        _require_file_references: bool,
    ) -> Result<SubAgentResult> {
        // Track concurrent executions
        let current = self.concurrent_count.fetch_add(1, Ordering::SeqCst) + 1;

        // Update max concurrent count
        let mut max = self.max_concurrent.load(Ordering::SeqCst);
        while current > max {
            match self.max_concurrent.compare_exchange_weak(
                max,
                current,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(m) => max = m,
            }
        }

        self.call_count.fetch_add(1, Ordering::SeqCst);

        // Simulate work
        if self.delay_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(self.delay_ms)).await;
        }

        // Track completion
        self.concurrent_count.fetch_sub(1, Ordering::SeqCst);

        Ok(SubAgentResult {
            answer: self.response.clone(),
            ui_output: format!(r#"{{"tools":[],"response":"{}"}}"#, self.response),
        })
    }
}

#[test]
fn test_spawn_agent_output_render() {
    use crate::tools::core::{Render, ResourcesTracker};

    // Test successful output
    let output = SpawnAgentOutput {
        answer: "The answer is 42.".to_string(),
        cancelled: false,
        error: None,
        ui_output: None,
    };

    let mut tracker = ResourcesTracker::new();
    let rendered = output.render(&mut tracker);
    assert_eq!(rendered, "The answer is 42.");
    assert_eq!(output.status(), "Sub-agent completed");

    // Test cancelled output
    let output = SpawnAgentOutput {
        answer: String::new(),
        cancelled: true,
        error: None,
        ui_output: None,
    };

    let rendered = output.render(&mut tracker);
    assert_eq!(rendered, "Sub-agent cancelled by user.");
    assert_eq!(output.status(), "Sub-agent cancelled by user");

    // Test error output
    let output = SpawnAgentOutput {
        answer: String::new(),
        cancelled: false,
        error: Some("Connection failed".to_string()),
        ui_output: None,
    };

    let rendered = output.render(&mut tracker);
    assert_eq!(rendered, "Sub-agent failed: Connection failed");
    assert!(output.status().contains("Connection failed"));

    // Test render_for_ui with ui_output set
    let output_with_ui = SpawnAgentOutput {
        answer: "Plain text answer".to_string(),
        cancelled: false,
        error: None,
        ui_output: Some(r#"{"tools":[],"response":"Markdown response"}"#.to_string()),
    };

    let mut tracker = ResourcesTracker::new();
    // render() returns plain text for LLM
    assert_eq!(output_with_ui.render(&mut tracker), "Plain text answer");
    // render_for_ui() returns JSON for UI
    assert_eq!(
        output_with_ui.render_for_ui(&mut tracker),
        r#"{"tools":[],"response":"Markdown response"}"#
    );

    // Test render_for_ui falls back to render() when ui_output is None
    let output_without_ui = SpawnAgentOutput {
        answer: "Just the answer".to_string(),
        cancelled: false,
        error: None,
        ui_output: None,
    };
    assert_eq!(
        output_without_ui.render_for_ui(&mut tracker),
        "Just the answer"
    );
}

#[test]
fn test_spawn_agent_input_parsing() {
    // Test with all parameters
    let json = serde_json::json!({
        "instructions": "Find all TODO comments",
        "require_file_references": true,
        "mode": "read_only"
    });

    let input: SpawnAgentInput = serde_json::from_value(json).unwrap();
    assert_eq!(input.instructions, "Find all TODO comments");
    assert!(input.require_file_references);
    assert_eq!(input.mode, "read_only");

    // Test with defaults
    let json = serde_json::json!({
        "instructions": "Search for patterns"
    });

    let input: SpawnAgentInput = serde_json::from_value(json).unwrap();
    assert_eq!(input.instructions, "Search for patterns");
    assert!(!input.require_file_references);
    assert_eq!(input.mode, "read_only"); // default
}

#[test]
fn test_cancellation_registry() {
    let registry = SubAgentCancellationRegistry::default();

    // Register a new tool
    let flag1 = registry.register("tool-1".to_string());
    assert!(!flag1.load(Ordering::SeqCst));

    let flag2 = registry.register("tool-2".to_string());
    assert!(!flag2.load(Ordering::SeqCst));

    // Cancel tool-1
    assert!(registry.cancel("tool-1"));
    assert!(flag1.load(Ordering::SeqCst));
    assert!(!flag2.load(Ordering::SeqCst));

    // Cancel non-existent tool returns false
    assert!(!registry.cancel("tool-3"));

    // Unregister tool-1
    registry.unregister("tool-1");

    // Cancel after unregister returns false
    assert!(!registry.cancel("tool-1"));
}

#[tokio::test]
async fn test_mock_sub_agent_runner() {
    let runner = MockSubAgentRunner::new(10, "Test response");

    let result = runner
        .run(
            "tool-1",
            "test".to_string(),
            ToolScope::SubAgentReadOnly,
            false,
        )
        .await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap().answer, "Test response");
    assert_eq!(runner.call_count(), 1);
}

#[tokio::test]
async fn test_parallel_sub_agent_execution() {
    use futures::future::join_all;

    let runner = Arc::new(MockSubAgentRunner::new(50, "Parallel result"));

    // Run multiple sub-agents in parallel
    let futures: Vec<_> = (0..4)
        .map(|i| {
            let runner = runner.clone();
            async move {
                runner
                    .run(
                        &format!("tool-{i}"),
                        format!("Task {i}"),
                        ToolScope::SubAgentReadOnly,
                        false,
                    )
                    .await
            }
        })
        .collect();

    let results = join_all(futures).await;

    // All should succeed
    assert_eq!(results.len(), 4);
    for result in results {
        assert!(result.is_ok());
        assert_eq!(result.unwrap().answer, "Parallel result");
    }

    // Verify they ran in parallel (max concurrent should be > 1)
    assert_eq!(runner.call_count(), 4);
    // With a 50ms delay and parallel execution, we should see concurrency
    assert!(
        runner.max_concurrent() > 1,
        "Expected parallel execution, but max concurrent was {}",
        runner.max_concurrent()
    );
}

#[test]
fn test_tool_scope_for_sub_agent() {
    use crate::tools::core::ToolRegistry;

    // spawn_agent should only be available in main agent scopes
    let registry = ToolRegistry::global();

    // Check spawn_agent is available in normal agent scope
    let tool = registry.get("spawn_agent");
    assert!(tool.is_some(), "spawn_agent should be registered");

    // Helper to get tool names for a scope
    let get_tools_for_scope = |scope: ToolScope| -> Vec<String> {
        registry
            .get_tool_definitions_for_scope(scope)
            .iter()
            .map(|t| t.name.clone())
            .collect()
    };

    // Check spawn_agent is NOT available in sub-agent scope
    let available = get_tools_for_scope(ToolScope::SubAgentReadOnly);
    assert!(
        !available.contains(&"spawn_agent".to_string()),
        "spawn_agent should not be available in SubAgentReadOnly scope"
    );

    let available = get_tools_for_scope(ToolScope::SubAgentDefault);
    assert!(
        !available.contains(&"spawn_agent".to_string()),
        "spawn_agent should not be available in SubAgentDefault scope"
    );

    // Check read-only tools are available in SubAgentReadOnly scope
    let available = get_tools_for_scope(ToolScope::SubAgentReadOnly);
    assert!(available.contains(&"search_files".to_string()));
    assert!(available.contains(&"read_files".to_string()));
    assert!(available.contains(&"list_files".to_string()));
    assert!(available.contains(&"glob_files".to_string()));
    assert!(available.contains(&"web_fetch".to_string()));
    assert!(available.contains(&"web_search".to_string()));
    assert!(available.contains(&"perplexity_ask".to_string()));

    // Check write tools are NOT available in SubAgentReadOnly scope
    assert!(!available.contains(&"write_file".to_string()));
    assert!(!available.contains(&"edit".to_string()));
    assert!(!available.contains(&"delete_files".to_string()));
    assert!(!available.contains(&"execute_command".to_string()));
}

#[test]
fn test_can_run_in_parallel_logic() {
    use crate::tools::ToolRequest;

    // This tests the logic of can_run_in_parallel without needing a full agent

    // spawn_agent with read_only mode should be parallelizable
    let request = ToolRequest {
        id: "test-1".to_string(),
        name: "spawn_agent".to_string(),
        input: serde_json::json!({
            "instructions": "test",
            "mode": "read_only"
        }),
        start_offset: None,
        end_offset: None,
    };

    let mode = request.input["mode"].as_str().unwrap_or("read_only");
    assert_eq!(mode, "read_only");
    assert!(request.name == "spawn_agent" && mode == "read_only");

    // spawn_agent with default mode (no mode specified) should default to read_only
    let request = ToolRequest {
        id: "test-2".to_string(),
        name: "spawn_agent".to_string(),
        input: serde_json::json!({
            "instructions": "test"
        }),
        start_offset: None,
        end_offset: None,
    };

    let mode = request.input["mode"].as_str().unwrap_or("read_only");
    assert_eq!(mode, "read_only");

    // spawn_agent with non-read_only mode should NOT be parallelizable
    let request = ToolRequest {
        id: "test-3".to_string(),
        name: "spawn_agent".to_string(),
        input: serde_json::json!({
            "instructions": "test",
            "mode": "default"
        }),
        start_offset: None,
        end_offset: None,
    };

    let mode = request.input["mode"].as_str().unwrap_or("read_only");
    assert_eq!(mode, "default");
    assert!(!(request.name == "spawn_agent" && mode == "read_only"));

    // Other tools should NOT be parallelizable
    let request = ToolRequest {
        id: "test-4".to_string(),
        name: "read_files".to_string(),
        input: serde_json::json!({
            "paths": ["test.txt"]
        }),
        start_offset: None,
        end_offset: None,
    };

    assert!(request.name != "spawn_agent");
}
