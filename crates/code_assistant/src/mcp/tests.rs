use crate::config::{DefaultProjectManager, ProjectManager};
use crate::explorer::Explorer;
use crate::mcp::handler::MessageHandler;
use crate::tests::mocks::MockProjectManager;
use crate::utils::{CommandExecutor, DefaultCommandExecutor, MockWriter};
use anyhow::Result;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::fs;
use tokio::sync::Mutex;

// This helper function creates a test environment with a temporary directory,
// fills it with some test files and returns a MessageHandler
// that points to this project.
async fn setup_test_environment() -> Result<(TempDir, Arc<Mutex<Vec<String>>>, MessageHandler)> {
    // Create temporary directory for the test project
    let temp_dir = TempDir::new()?;
    let temp_path = temp_dir.path();

    // Create multiple test files in the temporary directory

    // Main file in the root directory
    let main_file_path = temp_path.join("main.txt");
    let main_content = "This is the main file.\nIt contains important information.\nThe third line contains more details.";
    fs::write(&main_file_path, main_content).await?;

    // Create "src" subdirectory
    let src_dir = temp_path.join("src");
    fs::create_dir(&src_dir).await?;

    // Files in the src directory
    let src_file1_path = src_dir.join("code.rs");
    let src_file1_content = "fn main() {\n    println!(\"Hello, World!\");\n    // TODO: Implement more functionality\n}";
    fs::write(&src_file1_path, src_file1_content).await?;

    let src_file2_path = src_dir.join("utils.rs");
    let src_file2_content = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn subtract(a: i32, b: i32) -> i32 {\n    a - b\n}";
    fs::write(&src_file2_path, src_file2_content).await?;

    // Create "docs" subdirectory
    let docs_dir = temp_path.join("docs");
    fs::create_dir(&docs_dir).await?;

    // Files in the docs directory
    let docs_file_path = docs_dir.join("readme.md");
    let docs_file_content = "# Project Documentation\n\nThis project demonstrates the use of tools in the MCP server.\n\n## Tested Tools\n\n- read_files\n- list_files\n- search_files";
    fs::write(&docs_file_path, docs_file_content).await?;

    // Now we create a real Explorer for our directory
    let explorer = Explorer::new(temp_path.to_path_buf());

    // Create the MockProjectManager that uses the real Explorer
    let project_manager = Box::new(MockProjectManager::default().with_project_path(
        "test-project",
        temp_path.to_path_buf(),
        Box::new(explorer),
    ));

    let command_executor: Box<dyn CommandExecutor> = Box::new(DefaultCommandExecutor);
    let mock_writer = MockWriter::new();
    let writer_messages = mock_writer.messages.clone();
    let message_writer = Box::new(mock_writer);

    // Create MessageHandler with our dependencies
    let handler =
        MessageHandler::with_dependencies(project_manager, command_executor, message_writer);

    Ok((temp_dir, writer_messages, handler))
}

// This test verifies that MessageHandler correctly uses the MessageWriter trait
#[tokio::test]
async fn test_message_handler_with_mock_writer() {
    // Create mock components
    let project_manager: Box<dyn ProjectManager> = Box::new(DefaultProjectManager::new());
    let command_executor: Box<dyn CommandExecutor> = Box::new(DefaultCommandExecutor);
    let mock_writer = MockWriter::new();
    let writer_messages = mock_writer.messages.clone();
    let message_writer = Box::new(mock_writer);

    // Create message handler with mocked dependencies
    let mut handler =
        MessageHandler::with_dependencies(project_manager, command_executor, message_writer);

    // Create a valid JSON-RPC message that will be handled
    let message = r#"{"jsonrpc": "2.0", "method": "resources/list", "id": 1}"#;

    // Handle the message
    handler.handle_message(message).await.unwrap();

    // Verify that a response was written
    let messages = writer_messages.lock().await;
    assert_eq!(messages.len(), 1);

    // Verify it contains a valid JSON-RPC response
    let response = &messages[0];
    assert!(response.contains(r#""jsonrpc":"2.0""#));
    assert!(response.contains(r#""id":1"#));
    assert!(response.contains(r#""result""#));
    assert!(response.contains(r#""resources""#));
}

// This test verifies that the tools/list endpoint works correctly
#[tokio::test]
async fn test_tools_list() {
    // Create mock components
    let project_manager: Box<dyn ProjectManager> = Box::new(DefaultProjectManager::new());
    let command_executor: Box<dyn CommandExecutor> = Box::new(DefaultCommandExecutor);
    let mock_writer = MockWriter::new();
    let writer_messages = mock_writer.messages.clone();
    let message_writer = Box::new(mock_writer);

    // Create message handler with mocked dependencies
    let mut handler =
        MessageHandler::with_dependencies(project_manager, command_executor, message_writer);

    // Create a tools/list JSON-RPC message
    let message = r#"{"jsonrpc": "2.0", "method": "tools/list", "id": 1}"#;

    // Handle the message
    handler.handle_message(message).await.unwrap();

    // Verify the response
    let messages = writer_messages.lock().await;
    assert_eq!(messages.len(), 1);

    // Parse response to verify structure
    let response_json: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();

    // Verify it contains a valid JSON-RPC response with tools
    assert_eq!(response_json["jsonrpc"], "2.0");
    assert_eq!(response_json["id"], 1);
    assert!(response_json["result"]["tools"].is_array());

    // Check that the tools array contains tools with expected fields
    let tools = response_json["result"]["tools"].as_array().unwrap();
    assert!(!tools.is_empty(), "Expected tools list to contain tools");

    // Check format of at least one tool
    let first_tool = &tools[0];
    assert!(first_tool.get("name").is_some(), "Tool should have a name");
    assert!(
        first_tool.get("description").is_some(),
        "Tool should have a description"
    );
    assert!(
        first_tool.get("inputSchema").is_some(),
        "Tool should have an inputSchema"
    );
}

// This test creates a temporary project and tests the read_files tool
#[tokio::test]
async fn test_read_files_tool() -> Result<()> {
    // Set up environment with temporary files and MessageHandler
    let (_temp_dir, writer_messages, mut handler) = setup_test_environment().await?;

    // Call read_files tool to read a file
    let tool_call_message = r#"{
        "jsonrpc": "2.0",
        "method": "tools/call",
        "id": 1,
        "params": {
            "name": "read_files",
            "arguments": {
                "project": "test-project",
                "paths": ["main.txt"]
            }
        }
    }"#;

    handler.handle_message(tool_call_message).await?;

    // Check the response
    let messages = writer_messages.lock().await;
    assert_eq!(messages.len(), 1);

    // Analyze the response
    let response: serde_json::Value = serde_json::from_str(&messages[0])?;

    // Check basic response structure
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert!(!response["result"]["isError"].as_bool().unwrap());

    // Check that the content contains our test file
    let content = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(content.contains("This is the main file"));
    assert!(content.contains("It contains important information"));

    // Test error handling with non-existent file

    let invalid_tool_call = r#"{
        "jsonrpc": "2.0",
        "method": "tools/call",
        "id": 2,
        "params": {
            "name": "read_files",
            "arguments": {
                "project": "test-project",
                "paths": ["nonexistent.txt"]
            }
        }
    }"#;

    // Release lock on messages, otherwise handle_message would block
    drop(messages);

    handler.handle_message(invalid_tool_call).await?;

    // Check the error response
    let messages = writer_messages.lock().await;

    assert_eq!(
        messages.len(),
        2,
        "There should be 2 responses in writer_messages"
    );

    let error_response: serde_json::Value = serde_json::from_str(&messages[1])?;

    // The response should indicate an error
    assert!(error_response["result"]["isError"].as_bool().unwrap());
    let error_text = error_response["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(error_text.contains("Failed to load 'nonexistent.txt' in project 'test-project'"));

    Ok(())
}

// This test verifies error handling for a non-existent tool
#[tokio::test]
async fn test_unknown_tool() -> Result<()> {
    // Set up environment with temporary files and MessageHandler
    let (_temp_dir, writer_messages, mut handler) = setup_test_environment().await?;

    // Call a non-existent tool
    let unknown_tool_message = r#"{
        "jsonrpc": "2.0",
        "method": "tools/call",
        "id": 1,
        "params": {
            "name": "nonexistent_tool",
            "arguments": {}
        }
    }"#;

    handler.handle_message(unknown_tool_message).await?;

    // Check the response
    let messages = writer_messages.lock().await;
    assert_eq!(messages.len(), 1);

    let response: serde_json::Value = serde_json::from_str(&messages[0])?;

    // Verify that it's an error response
    assert!(response.get("error").is_some());
    assert_eq!(response["error"]["code"], -32602); // Invalid params error code
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Tool not found"));

    Ok(())
}
