//! End-to-end tests against an in-process rmcp server connected over a
//! duplex stream — the same code path as a real stdio server, minus the
//! child process.

use crate::client::McpServerConnection;
use crate::config::McpServerConfig;
use crate::registry::{register_connection_tools, server_scope_capability, MCP_CAPABILITY};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, ListToolsResult,
    PaginatedRequestParams, Tool as McpToolDescriptor,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ServerHandler, ServiceExt};
use serde_json::json;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tools_core::registry::ToolRegistry;
use tools_core::render::ResourcesTracker;
use tools_core::tool::ToolContext;

/// An MCP server offering `echo` (mirrors its message) and `fail` (always
/// returns an error result).
struct TestServer;

fn schema(value: serde_json::Value) -> Arc<rmcp::model::JsonObject> {
    match value {
        serde_json::Value::Object(map) => Arc::new(map),
        _ => panic!("schema must be an object"),
    }
}

impl ServerHandler for TestServer {
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![
            McpToolDescriptor::new(
                "echo",
                "Echo a message back",
                schema(json!({
                    "type": "object",
                    "properties": { "message": { "type": "string" } },
                    "required": ["message"]
                })),
            ),
            McpToolDescriptor::new("fail", "Always fails", schema(json!({ "type": "object" }))),
        ]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match request.name.as_ref() {
            "echo" => {
                let message = request
                    .arguments
                    .as_ref()
                    .and_then(|arguments| arguments.get("message"))
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "echo: {message}"
                ))]))
            }
            "fail" => Ok(CallToolResult::error(vec![ContentBlock::text("it broke")])),
            other => Err(ErrorData::invalid_params(
                format!("unknown tool: {other}"),
                None,
            )),
        }
    }
}

/// Serve `TestServer` over one end of a duplex pipe and connect a
/// `McpServerConnection` to the other.
async fn connect_test_server() -> (Arc<McpServerConnection>, JoinHandle<()>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        let service = TestServer
            .serve(server_io)
            .await
            .expect("test server failed to start");
        let _ = service.waiting().await;
    });
    let connection = McpServerConnection::connect_transport("test", client_io)
        .await
        .expect("client failed to connect");
    (Arc::new(connection), server_task)
}

fn server_config(json: serde_json::Value) -> McpServerConfig {
    serde_json::from_value(json).unwrap()
}

/// A ToolContext for tools that don't touch the executor.
struct NoExecutor;

#[async_trait::async_trait]
impl command_executor::CommandExecutor for NoExecutor {
    async fn execute(
        &self,
        _command_line: &str,
        _working_dir: Option<&std::path::PathBuf>,
        _sandbox_request: Option<&command_executor::SandboxCommandRequest>,
    ) -> anyhow::Result<command_executor::CommandOutput> {
        unreachable!("MCP tools must not use the command executor")
    }

    async fn execute_streaming(
        &self,
        _command_line: &str,
        _working_dir: Option<&std::path::PathBuf>,
        _callback: Option<&dyn command_executor::StreamingCallback>,
        _sandbox_request: Option<&command_executor::SandboxCommandRequest>,
    ) -> anyhow::Result<command_executor::CommandOutput> {
        unreachable!("MCP tools must not use the command executor")
    }
}

fn test_context() -> ToolContext<'static> {
    static EXECUTOR: NoExecutor = NoExecutor;
    ToolContext {
        command_executor: &EXECUTOR,
        tool_id: None,
        session_id: None,
        permission_handler: None,
        extensions: None,
    }
}

#[tokio::test]
async fn registers_discovered_tools_with_capability_tags() {
    let (connection, _server) = connect_test_server().await;
    let mut registry = ToolRegistry::new();
    let config = server_config(json!({ "command": "unused" }));

    let registered =
        register_connection_tools(&mut registry, connection, &config, &["scope:agent"])
            .await
            .unwrap();

    assert_eq!(registered, ["mcp__test__echo", "mcp__test__fail"]);
    let tool = registry.get("mcp__test__echo").expect("echo registered");
    let spec = tool.spec();
    assert_eq!(spec.description, "Echo a message back");
    assert!(spec.has_capability(MCP_CAPABILITY));
    assert!(spec.has_capability(&server_scope_capability("test")));
    assert!(spec.has_capability("scope:agent"));
    assert_eq!(
        spec.parameters_schema["properties"]["message"]["type"],
        "string"
    );
}

#[tokio::test]
async fn invoke_round_trips_through_the_server() {
    let (connection, _server) = connect_test_server().await;
    let mut registry = ToolRegistry::new();
    let config = server_config(json!({ "command": "unused" }));
    register_connection_tools(&mut registry, connection, &config, &[])
        .await
        .unwrap();

    let tool = registry.get("mcp__test__echo").unwrap();
    let mut params = json!({ "message": "hello" });
    let output = tool.invoke(&mut test_context(), &mut params).await.unwrap();

    assert!(output.is_success());
    let rendered = output.as_render().render(&mut ResourcesTracker::new());
    assert_eq!(rendered, "echo: hello");
}

#[tokio::test]
async fn error_results_are_reported_as_failures() {
    let (connection, _server) = connect_test_server().await;
    let mut registry = ToolRegistry::new();
    let config = server_config(json!({ "command": "unused" }));
    register_connection_tools(&mut registry, connection, &config, &[])
        .await
        .unwrap();

    let tool = registry.get("mcp__test__fail").unwrap();
    let mut params = json!({});
    let output = tool.invoke(&mut test_context(), &mut params).await.unwrap();

    assert!(!output.is_success());
    let rendered = output.as_render().render(&mut ResourcesTracker::new());
    assert_eq!(rendered, "it broke");
}

#[tokio::test]
async fn disabled_tools_are_not_registered() {
    let (connection, _server) = connect_test_server().await;
    let mut registry = ToolRegistry::new();
    let config = server_config(json!({ "command": "unused", "disabled_tools": ["fail"] }));

    let registered = register_connection_tools(&mut registry, connection, &config, &[])
        .await
        .unwrap();

    assert_eq!(registered, ["mcp__test__echo"]);
    assert!(registry.get("mcp__test__fail").is_none());
}

#[tokio::test]
async fn dead_server_degrades_to_tool_error() {
    let (connection, server) = connect_test_server().await;
    let mut registry = ToolRegistry::new();
    let config = server_config(json!({ "command": "unused" }));
    register_connection_tools(&mut registry, connection, &config, &[])
        .await
        .unwrap();
    server.abort();
    let _ = server.await;

    let tool = registry.get("mcp__test__echo").unwrap();
    let mut params = json!({ "message": "anyone there?" });
    let output = tool.invoke(&mut test_context(), &mut params).await.unwrap();

    assert!(!output.is_success());
}

/// Smoke test against a real child process: code-assistant's own MCP server
/// mode. Ignored by default because it needs the workspace binary built
/// first (`cargo build -p code-assistant --no-default-features`).
#[tokio::test]
#[ignore = "needs a built code-assistant binary in target/debug"]
async fn connects_to_a_real_stdio_server() {
    let binary =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/code-assistant");
    assert!(
        binary.exists(),
        "build the code-assistant binary before running this test"
    );

    let config = server_config(json!({
        "command": binary.to_string_lossy(),
        "args": ["server"]
    }));
    let connection = McpServerConnection::connect("self", &config).await.unwrap();
    let tools = connection.list_tools().await.unwrap();
    assert!(
        tools.iter().any(|tool| tool.name == "read_files"),
        "expected code-assistant's MCP server tools, got: {:?}",
        tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>()
    );
    connection.shutdown().await.unwrap();
}

#[tokio::test]
async fn non_object_params_are_rejected() {
    let (connection, _server) = connect_test_server().await;
    let mut registry = ToolRegistry::new();
    let config = server_config(json!({ "command": "unused" }));
    register_connection_tools(&mut registry, connection, &config, &[])
        .await
        .unwrap();

    let tool = registry.get("mcp__test__echo").unwrap();
    let mut params = json!("not an object");
    let result = tool.invoke(&mut test_context(), &mut params).await;

    assert!(result.is_err());
}
