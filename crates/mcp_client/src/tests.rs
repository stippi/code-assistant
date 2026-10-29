//! End-to-end tests against an in-process rmcp server connected over a
//! duplex stream — the same code path as a real stdio server, minus the
//! child process.

use crate::client::McpServerConnection;
use crate::config::McpServerConfig;
use crate::registry::{MCP_CAPABILITY, register_connection_tools, server_scope_capability};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    ListToolsResult, PaginatedRequestParams, Tool as McpToolDescriptor,
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
    ) -> Result<CallToolResponse, ErrorData> {
        match request.name.as_ref() {
            "echo" => {
                let message = request
                    .arguments
                    .as_ref()
                    .and_then(|arguments| arguments.get("message"))
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                Ok(
                    CallToolResult::success(vec![ContentBlock::text(format!("echo: {message}"))])
                        .into(),
                )
            }
            "fail" => Ok(CallToolResult::error(vec![ContentBlock::text("it broke")]).into()),
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

#[tokio::test]
async fn closing_ends_the_session() {
    let (connection, _server) = connect_test_server().await;
    assert!(!connection.is_closed());
    connection.close();
    assert!(connection.is_closed());
    assert!(echo(&connection, "still there?").await.is_err());
}

#[tokio::test]
async fn a_vanished_server_reads_as_closed() {
    let (connection, server) = connect_test_server().await;
    server.abort();
    let _ = server.await;
    // The client notices the closed pipe on its own, without a call.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !connection.is_closed() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the connection should notice its server is gone");
}

/// A stdio server that dies before the handshake explains itself on stderr;
/// that output belongs in the connection error, not just in the app's log.
#[tokio::test]
#[cfg(unix)]
async fn stdio_failure_reports_the_servers_stderr() {
    let config = server_config(json!({
        "command": "sh",
        "args": ["-c", "echo 'first line' >&2; echo 'error: no such package' >&2; exit 1"]
    }));
    let error = McpServerConnection::connect("broken", &config, None)
        .await
        .err()
        .expect("a server that exits must not connect");
    let message = format!("{error:#}");
    assert!(
        message.contains("first line\nerror: no such package"),
        "expected the server's stderr in the error, got: {message}"
    );
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
    let connection = McpServerConnection::connect("self", &config, None)
        .await
        .unwrap();
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

/// Serve `TestServer` over the streamable HTTP transport on an ephemeral
/// localhost port, returning its `/mcp` URL and the server task (aborted on
/// drop).
async fn spawn_http_server() -> (String, tokio::task::JoinHandle<()>) {
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::tower::{
        StreamableHttpServerConfig, StreamableHttpService,
    };

    let service = StreamableHttpService::new(
        || Ok(TestServer),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{addr}/mcp"), server)
}

#[tokio::test]
async fn connects_and_calls_over_http() {
    let (url, _server) = spawn_http_server().await;
    let config = server_config(json!({ "url": url }));

    let connection = Arc::new(
        McpServerConnection::connect("http-test", &config, None)
            .await
            .expect("client failed to connect over HTTP"),
    );
    let mut registry = ToolRegistry::new();
    let registered = register_connection_tools(&mut registry, connection, &config, &[])
        .await
        .unwrap();
    assert_eq!(registered, ["mcp__http-test__echo", "mcp__http-test__fail"]);

    let tool = registry.get("mcp__http-test__echo").unwrap();
    let mut params = json!({ "message": "over http" });
    let output = tool.invoke(&mut test_context(), &mut params).await.unwrap();
    assert!(output.is_success());
    let rendered = output.as_render().render(&mut ResourcesTracker::new());
    assert_eq!(rendered, "echo: over http");
}

/// Serve a minimal HTTP endpoint that answers every request with `401` and a
/// `WWW-Authenticate` challenge — the reactive OAuth trigger. Returns its
/// `/mcp` URL and the server task (aborted on drop).
async fn spawn_auth_required_server() -> (String, tokio::task::JoinHandle<()>) {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    async fn unauthorized() -> impl IntoResponse {
        (
            StatusCode::UNAUTHORIZED,
            [(
                "WWW-Authenticate",
                "Bearer resource_metadata=\"https://auth.example.com/.well-known/oauth-protected-resource\"",
            )],
            "authorization required",
        )
    }

    let router = axum::Router::new().route("/mcp", axum::routing::any(unauthorized));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{addr}/mcp"), server)
}

/// A server that demands OAuth must fail the connect with a *typed*
/// [`AuthorizationRequired`] (carrying the challenge and server name), not a
/// generic error — that is what lets an embedder offer an "Authenticate"
/// action instead of a plain retry.
#[tokio::test]
async fn http_server_requiring_auth_surfaces_authorization_required() {
    let (url, _server) = spawn_auth_required_server().await;
    let config = server_config(json!({ "url": url }));

    let error = match McpServerConnection::connect("sap", &config, None).await {
        Ok(_) => panic!("connect must fail when the server demands authorization"),
        Err(error) => error,
    };

    let auth = error
        .downcast_ref::<crate::auth::AuthorizationRequired>()
        .unwrap_or_else(|| panic!("expected AuthorizationRequired, got: {error:#}"));
    assert_eq!(auth.server, "sap");
    assert!(
        auth.challenge.contains("resource_metadata"),
        "the WWW-Authenticate challenge is carried through: {}",
        auth.challenge
    );
}

/// A stored login the server no longer accepts (the token expired and cannot
/// be refreshed) fails the connect as [`AuthorizationRequired`], like no
/// login at all, so the embedder offers a new login instead of a dead end.
#[tokio::test]
async fn unusable_stored_login_surfaces_authorization_required() {
    let (url, _server) = spawn_auth_required_server().await;
    let config = server_config(json!({ "url": url }));
    let credentials: rmcp::transport::auth::StoredCredentials = serde_json::from_value(json!({
        "client_id": "client-123",
        "token_response": {
            "access_token": "stale",
            "token_type": "bearer",
            "expires_in": 1,
            "refresh_token": "revoked"
        },
        "token_received_at": 1
    }))
    .unwrap();
    let store = rmcp::transport::auth::InMemoryCredentialStore::new();
    rmcp::transport::auth::CredentialStore::save(&store, credentials)
        .await
        .unwrap();
    let store: Arc<dyn rmcp::transport::auth::CredentialStore> = Arc::new(store);

    let error = match McpServerConnection::connect("remote", &config, Some(store)).await {
        Ok(_) => panic!("connect must fail when the stored login is unusable"),
        Err(error) => error,
    };
    assert!(
        error
            .downcast_ref::<crate::auth::AuthorizationRequired>()
            .is_some(),
        "expected AuthorizationRequired, got: {error:#}"
    );
}

/// OAuth authorization only applies to HTTP servers; asking to authenticate a
/// stdio server is a configuration error, surfaced without touching the
/// network.
#[tokio::test]
async fn authenticate_rejects_stdio_servers() {
    use crate::auth::{AuthorizationOutcome, OAuthAuthorizer};

    struct NeverCalled;
    #[async_trait::async_trait]
    impl OAuthAuthorizer for NeverCalled {
        fn redirect_uri(&self) -> String {
            "http://127.0.0.1:0/callback".to_string()
        }
        async fn authorize(&self, _url: String) -> anyhow::Result<AuthorizationOutcome> {
            panic!("authorizer must not be invoked for a stdio server")
        }
    }

    let config = server_config(json!({ "command": "npx" }));
    let store: Arc<dyn rmcp::transport::auth::CredentialStore> =
        Arc::new(rmcp::transport::auth::InMemoryCredentialStore::new());

    let error = crate::client::authenticate_http_server(
        "stdio-srv",
        &config,
        store,
        &NeverCalled,
        "code-assistant",
    )
    .await
    .expect_err("authenticating a stdio server must fail");
    assert!(
        error.to_string().contains("not an HTTP server"),
        "unexpected error: {error:#}"
    );
}

/// An HTTP MCP server (`server` makes its handler) whose "login" can be
/// expired: while `expired` is set, every request gets a `401` challenge.
/// Returns its URL, the flag and the server task.
async fn spawn_expiring_http_server<S: ServerHandler>(
    server: fn() -> S,
) -> (
    String,
    Arc<std::sync::atomic::AtomicBool>,
    tokio::task::JoinHandle<()>,
) {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::tower::{
        StreamableHttpServerConfig, StreamableHttpService,
    };
    use std::sync::atomic::Ordering;

    let expired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let service = StreamableHttpService::new(
        move || Ok(server()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let flag = expired.clone();
    let router =
        axum::Router::new()
            .nest_service("/mcp", service)
            .layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let flag = flag.clone();
                    async move {
                        if flag.load(Ordering::SeqCst) {
                            return (
                                StatusCode::UNAUTHORIZED,
                                [("WWW-Authenticate", "Bearer error=\"invalid_token\"")],
                                "token expired",
                            )
                                .into_response();
                        }
                        next.run(request).await
                    }
                },
            ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{addr}/mcp"), expired, server)
}

/// "Logs in again" by lifting the expiry, counting how often it was asked.
struct FakeReauthorizer {
    expired: Arc<std::sync::atomic::AtomicBool>,
    calls: std::sync::atomic::AtomicUsize,
    /// Decline instead, as with automatic re-authorization switched off.
    decline: bool,
}

#[async_trait::async_trait]
impl crate::auth::Reauthorizer for FakeReauthorizer {
    async fn reauthorize(&self, _server: &str) -> anyhow::Result<()> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.decline {
            anyhow::bail!("automatic re-authorization is off");
        }
        self.expired
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

async fn echo(connection: &McpServerConnection, message: &str) -> anyhow::Result<String> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("message".into(), json!(message));
    let result = connection.call_tool("echo", Some(arguments)).await?;
    Ok(format!("{:?}", result.content))
}

/// A login that expires while the connection is in use is renewed through the
/// reauthorizer, and the rejected call is retried on the new session.
#[tokio::test]
async fn expired_login_mid_session_reauthorizes_and_retries() {
    use std::sync::atomic::Ordering;
    let (url, expired, _server) = spawn_expiring_http_server(|| TestServer).await;
    let config = server_config(json!({ "url": url }));
    let reauthorizer = Arc::new(FakeReauthorizer {
        expired: expired.clone(),
        calls: Default::default(),
        decline: false,
    });
    let connection = McpServerConnection::connect("expiring", &config, None)
        .await
        .unwrap()
        .with_reauthorizer(reauthorizer.clone());
    assert!(
        echo(&connection, "before")
            .await
            .unwrap()
            .contains("before")
    );

    expired.store(true, Ordering::SeqCst);
    let after = echo(&connection, "after")
        .await
        .expect("the call recovers after re-authorization");
    assert!(after.contains("after"), "{after}");
    assert_eq!(reauthorizer.calls.load(Ordering::SeqCst), 1);

    // The new session keeps working without asking again.
    assert!(echo(&connection, "again").await.unwrap().contains("again"));
    assert_eq!(reauthorizer.calls.load(Ordering::SeqCst), 1);
}

/// A declined re-authorization fails the call with the reauthorizer's reason.
#[tokio::test]
async fn declined_reauthorization_fails_the_call_with_its_reason() {
    use std::sync::atomic::Ordering;
    let (url, expired, _server) = spawn_expiring_http_server(|| TestServer).await;
    let config = server_config(json!({ "url": url }));
    let reauthorizer = Arc::new(FakeReauthorizer {
        expired: expired.clone(),
        calls: Default::default(),
        decline: true,
    });
    let connection = McpServerConnection::connect("expiring", &config, None)
        .await
        .unwrap()
        .with_reauthorizer(reauthorizer.clone());
    expired.store(true, Ordering::SeqCst);

    let error = echo(&connection, "x").await.unwrap_err();
    assert!(
        format!("{error:#}").contains("automatic re-authorization is off"),
        "{error:#}"
    );
    assert_eq!(reauthorizer.calls.load(Ordering::SeqCst), 1);
}

/// An MCP server with resources: a text file, a PNG, a JSON blob, opaque
/// binary data, one meant for the user only, a template — and a tool
/// returning a resource link and an embedded resource.
struct ResourceServer;

const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

impl ServerHandler for ResourceServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![
            McpToolDescriptor::new(
                "find_notes",
                "Find notes",
                schema(json!({ "type": "object" })),
            ),
        ]))
    }

    async fn call_tool(
        &self,
        _request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        Ok(CallToolResult::success(vec![
            ContentBlock::resource_link(
                rmcp::model::Resource::new("notes://todo", "todo").with_description("Open tasks"),
            ),
            ContentBlock::embedded_text("notes://inline", "inline note"),
        ])
        .into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, ErrorData> {
        let mut user_only = rmcp::model::Resource::new("notes://private", "private");
        user_only.annotations =
            Some(serde_json::from_value(json!({ "audience": ["user"] })).unwrap());
        Ok(rmcp::model::ListResourcesResult::with_all_items(vec![
            rmcp::model::Resource::new("file:///readme.md", "readme.md")
                .with_description("Project overview"),
            rmcp::model::Resource::new("file:///logo.png", "logo.png"),
            user_only,
        ]))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListResourceTemplatesResult, ErrorData> {
        let template: rmcp::model::ResourceTemplate = serde_json::from_value(json!({
            "uriTemplate": "notes://{id}",
            "name": "note",
            "description": "A note by id"
        }))
        .unwrap();
        Ok(rmcp::model::ListResourceTemplatesResult::with_all_items(
            vec![template],
        ))
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, ErrorData> {
        use base64::Engine;
        use rmcp::model::ResourceContents;
        let blob =
            |uri: &str, mime_type: &str, bytes: &[u8]| ResourceContents::BlobResourceContents {
                uri: uri.to_string(),
                mime_type: Some(mime_type.to_string()),
                blob: base64::engine::general_purpose::STANDARD.encode(bytes),
                meta: None,
            };
        let contents = match request.uri.as_str() {
            "file:///readme.md" => vec![ResourceContents::text("# Readme", "file:///readme.md")],
            "file:///logo.png" => vec![ResourceContents::BlobResourceContents {
                uri: "file:///logo.png".to_string(),
                mime_type: Some("image/png".to_string()),
                blob: PNG_1X1.to_string(),
                meta: None,
            }],
            "data://config" => vec![blob("data://config", "application/json", b"{\"a\":1}")],
            "data://binary" => vec![blob(
                "data://binary",
                "application/octet-stream",
                &[0xff, 0xfe, 0x00],
            )],
            "notes://both" => vec![
                ResourceContents::text("one", "notes://1"),
                ResourceContents::text("two", "notes://2"),
            ],
            other => {
                return Err(ErrorData::resource_not_found(
                    format!("no resource {other}"),
                    None,
                ));
            }
        };
        Ok(rmcp::model::ReadResourceResult::new(contents).into())
    }
}

async fn connect_resource_server() -> (Arc<McpServerConnection>, JoinHandle<()>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        let service = ResourceServer
            .serve(server_io)
            .await
            .expect("resource server failed to start");
        let _ = service.waiting().await;
    });
    let connection = McpServerConnection::connect_transport("docs", client_io)
        .await
        .expect("client failed to connect");
    (Arc::new(connection), server_task)
}

/// Read `uri` and reduce its contents to parts.
async fn read(connection: &McpServerConnection, uri: &str) -> Vec<crate::ResourcePart> {
    connection
        .read_resource(uri)
        .await
        .unwrap()
        .contents
        .iter()
        .map(crate::ResourcePart::from_contents)
        .collect()
}

#[tokio::test]
async fn servers_with_resources_are_left_with_the_registry() {
    let (connection, _server) = connect_resource_server().await;
    let mut registry = ToolRegistry::new();
    let config = server_config(json!({ "command": "unused" }));

    let registered = register_connection_tools(&mut registry, connection, &config, &[])
        .await
        .unwrap();

    // Resources add no tools…
    assert_eq!(registered, ["mcp__docs__find_notes"]);
    // …but the server and what it listed travel with the registry.
    let servers = registry
        .extension::<crate::McpResourceServers>()
        .expect("resource servers left with the registry");
    let docs = servers.get("docs").expect("docs offers resources");
    assert_eq!(docs.catalog.resources.len(), 3);
    assert_eq!(docs.catalog.templates.len(), 1);
}

#[tokio::test]
async fn servers_without_resources_are_not_left_with_the_registry() {
    let (connection, _server) = connect_test_server().await;
    let mut registry = ToolRegistry::new();
    let config = server_config(json!({ "command": "unused" }));

    register_connection_tools(&mut registry, connection, &config, &[])
        .await
        .unwrap();

    assert!(registry.extension::<crate::McpResourceServers>().is_none());
}

#[tokio::test]
async fn catalogs_render_listings_and_summaries() {
    let (connection, _server) = connect_resource_server().await;
    let catalog = crate::ResourceCatalog::discover(&connection).await.unwrap();

    let listing = catalog.render_listing("");
    assert!(
        listing.contains("- file:///logo.png — logo.png"),
        "{listing}"
    );
    assert!(listing.contains("- notes://private — private"), "{listing}");
    assert!(
        listing.contains("- notes://{id} — note: A note by id"),
        "{listing}"
    );
    let narrowed = catalog.render_listing("file:///r");
    assert!(narrowed.contains("file:///readme.md"), "{narrowed}");
    assert!(!narrowed.contains("logo.png"), "{narrowed}");

    // The summary leaves out what is meant for the user only.
    let summary = catalog.render_summary("list them").unwrap();
    assert!(
        summary.contains("- file:///readme.md — readme.md: Project overview"),
        "{summary}"
    );
    assert!(summary.contains("notes://{id}"), "{summary}");
    assert!(!summary.contains("notes://private"), "{summary}");
}

#[tokio::test]
async fn reads_become_text_images_or_unusable_parts() {
    use crate::ResourcePart;
    let (connection, _server) = connect_resource_server().await;

    assert!(matches!(
        read(&connection, "file:///readme.md").await.as_slice(),
        [ResourcePart::Text { text, .. }] if text == "# Readme"
    ));
    assert!(matches!(
        read(&connection, "data://config").await.as_slice(),
        [ResourcePart::Text { text, .. }] if text == "{\"a\":1}"
    ));
    assert!(matches!(
        read(&connection, "file:///logo.png").await.as_slice(),
        [ResourcePart::Image { media_type, .. }] if media_type == "image/png"
    ));
    let binary = read(&connection, "data://binary").await;
    assert_eq!(
        binary[0].unusable_reason().unwrap(),
        "binary content (application/octet-stream, 3 bytes) cannot be shown"
    );
    assert_eq!(read(&connection, "notes://both").await.len(), 2);

    let missing = connection.read_resource("notes://nope").await.unwrap_err();
    assert!(
        format!("{missing:#}").contains("no resource notes://nope"),
        "{missing:#}"
    );
}

#[tokio::test]
async fn tool_results_resolve_resource_links_and_embedded_resources() {
    let (connection, _server) = connect_resource_server().await;
    let mut registry = ToolRegistry::new();
    let config = server_config(json!({ "command": "unused" }));
    register_connection_tools(&mut registry, connection, &config, &[])
        .await
        .unwrap();

    let tool = registry.get("mcp__docs__find_notes").unwrap();
    let output = tool
        .invoke(&mut test_context(), &mut json!({}))
        .await
        .unwrap();
    let rendered = output.as_render().render(&mut ResourcesTracker::new());

    assert_eq!(
        rendered,
        "[resource link] notes://todo — todo: Open tasks\n--- resource notes://inline (text/plain) ---\ninline note"
    );
}

/// Listing resources recovers an expired login the way a tool call does.
#[tokio::test]
async fn listing_resources_reauthorizes_an_expired_login() {
    use std::sync::atomic::Ordering;
    let (url, expired, _server) = spawn_expiring_http_server(|| ResourceServer).await;
    let config = server_config(json!({ "url": url }));
    let reauthorizer = Arc::new(FakeReauthorizer {
        expired: expired.clone(),
        calls: Default::default(),
        decline: false,
    });
    let connection = McpServerConnection::connect("expiring", &config, None)
        .await
        .unwrap()
        .with_reauthorizer(reauthorizer.clone());
    expired.store(true, Ordering::SeqCst);

    let catalog = crate::ResourceCatalog::discover(&connection)
        .await
        .expect("listing recovers after re-authorization");
    assert_eq!(catalog.resources.len(), 3);
    assert_eq!(catalog.templates.len(), 1);
    assert_eq!(reauthorizer.calls.load(Ordering::SeqCst), 1);
}

/// A server whose resource listing never answers.
struct HangingResourceServer;

impl ServerHandler for HangingResourceServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, ErrorData> {
        std::future::pending().await
    }
}

/// Registration waits only briefly for a resource listing: the server is
/// still offered (with an empty catalog), its tools registered.
#[tokio::test(start_paused = true)]
async fn a_hanging_resource_listing_does_not_hold_up_registration() {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let service = HangingResourceServer.serve(server_io).await.unwrap();
        let _ = service.waiting().await;
    });
    let connection = McpServerConnection::connect_transport("slow", client_io)
        .await
        .unwrap();
    let mut registry = ToolRegistry::new();
    let config = server_config(json!({ "command": "unused" }));

    let started = tokio::time::Instant::now();
    register_connection_tools(&mut registry, Arc::new(connection), &config, &[])
        .await
        .unwrap();

    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "waited {:?}",
        started.elapsed()
    );
    let servers = registry.extension::<crate::McpResourceServers>().unwrap();
    assert!(servers.get("slow").unwrap().catalog.resources.is_empty());
}
