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

// Diese Hilfsfunktion erstellt eine Testumgebung mit einem temporären Verzeichnis,
// befüllt es mit einigen Testdateien und gibt einen MessageHandler zurück,
// der auf dieses Projekt zeigt.
async fn setup_test_environment() -> Result<(TempDir, Arc<Mutex<Vec<String>>>, MessageHandler)> {
    // Temporäres Verzeichnis für das Testprojekt erstellen
    let temp_dir = TempDir::new()?;
    let temp_path = temp_dir.path();

    // Mehrere Testdateien im temporären Verzeichnis anlegen

    // Hauptdatei im Wurzelverzeichnis
    let main_file_path = temp_path.join("main.txt");
    let main_content = "Dies ist die Hauptdatei.\nSie enthält wichtige Informationen.\nDie dritte Zeile enthält weitere Details.";
    fs::write(&main_file_path, main_content).await?;

    // Unterordner "src" erstellen
    let src_dir = temp_path.join("src");
    fs::create_dir(&src_dir).await?;

    // Dateien im src-Verzeichnis
    let src_file1_path = src_dir.join("code.rs");
    let src_file1_content = "fn main() {\n    println!(\"Hello, World!\");\n    // TODO: Implement more functionality\n}";
    fs::write(&src_file1_path, src_file1_content).await?;

    let src_file2_path = src_dir.join("utils.rs");
    let src_file2_content = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn subtract(a: i32, b: i32) -> i32 {\n    a - b\n}";
    fs::write(&src_file2_path, src_file2_content).await?;

    // Unterordner "docs" erstellen
    let docs_dir = temp_path.join("docs");
    fs::create_dir(&docs_dir).await?;

    // Dateien im docs-Verzeichnis
    let docs_file_path = docs_dir.join("readme.md");
    let docs_file_content = "# Projektdokumentation\n\nDieses Projekt demonstriert die Verwendung der Tools im MCP-Server.\n\n## Getestete Tools\n\n- read_files\n- list_files\n- search_files";
    fs::write(&docs_file_path, docs_file_content).await?;

    // Jetzt erstellen wir einen echten Explorer für unser Verzeichnis
    let explorer = Explorer::new(temp_path.to_path_buf());

    // Erstelle den MockProjectManager, der den echten Explorer verwendet
    let project_manager = Box::new(MockProjectManager::default().with_project(
        "test-project",
        temp_path.to_path_buf(),
        Box::new(explorer),
    ));

    let command_executor: Box<dyn CommandExecutor> = Box::new(DefaultCommandExecutor);
    let mock_writer = MockWriter::new();
    let writer_messages = mock_writer.messages.clone();
    let message_writer = Box::new(mock_writer);

    // MessageHandler mit unseren Abhängigkeiten erstellen
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
    // Setup-Umgebung mit temporären Dateien und MessageHandler
    let (_temp_dir, writer_messages, mut handler) = setup_test_environment().await?;

    // read_files-Tool aufrufen, um eine Datei zu lesen
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

    // Antwort überprüfen
    let messages = writer_messages.lock().await;
    assert_eq!(messages.len(), 1);

    // Antwort analysieren
    let response: serde_json::Value = serde_json::from_str(&messages[0])?;

    // Grundlegende Antwortstruktur überprüfen
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert!(!response["result"]["isError"].as_bool().unwrap());

    // Prüfen, dass der Inhalt unsere Testdatei enthält
    let content = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(content.contains("Dies ist die Hauptdatei"));
    assert!(content.contains("Sie enthält wichtige Informationen"));

    // Testen der Fehlerbehandlung mit nicht existierender Datei

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

    // Fehlerantwort überprüfen
    let messages = writer_messages.lock().await;

    assert_eq!(
        messages.len(),
        2,
        "Es sollten 2 Antworten im writer_messages sein"
    );

    let error_response: serde_json::Value = serde_json::from_str(&messages[1])?;

    // Die Antwort sollte einen Fehler anzeigen
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
    // Setup-Umgebung mit temporären Dateien und MessageHandler
    let (_temp_dir, writer_messages, mut handler) = setup_test_environment().await?;

    // Nicht-existierendes Tool aufrufen
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

    // Antwort überprüfen
    let messages = writer_messages.lock().await;
    assert_eq!(messages.len(), 1);

    let response: serde_json::Value = serde_json::from_str(&messages[0])?;

    // Überprüfen, ob es eine Fehlerantwort ist
    assert!(response.get("error").is_some());
    assert_eq!(response["error"]["code"], -32602); // Invalid params error code
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Tool not found"));

    Ok(())
}
