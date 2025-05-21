use crate::config::{DefaultProjectManager, ProjectManager};
use crate::mcp::handler::MessageHandler;
use crate::utils::{CommandExecutor, DefaultCommandExecutor, MockWriter};

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
