//! Integration tests for the code-assistant
//!
//! These tests demonstrate the full workflow with recorded LLM sessions

use crate::{
    llm::{LLMProvider, LLMRequest, StreamingCallback, StreamingChunk},
    tests::recording_player::RecordingPlayer,
    ui::{streaming::StreamProcessor, streaming_test::TestUI, UserInterface},
};
use anyhow::Result;
use std::sync::{Arc, Mutex};

/// Test that demonstrates loading a recorded session and playing it through the UI
#[tokio::test]
async fn test_playback_recorded_session() -> Result<()> {
    // Using an existing recording file from sessions directory
    let recording_path = "sessions/assess-code-quality.json";

    // Load the recording
    let player = RecordingPlayer::from_file(recording_path)?;

    // Create a mock provider with the first session
    let mut provider = player.create_mock_provider(0)?;

    // Optionally turn off timing simulation for faster tests
    provider.set_simulate_timing(false);

    // Create a test UI that captures display fragments
    let test_ui = TestUI::new();
    let ui_arc = Arc::new(Box::new(test_ui.clone()) as Box<dyn UserInterface>);

    // Create a stream processor to handle the chunks
    let stream_processor = Arc::new(Mutex::new(StreamProcessor::new(ui_arc.clone())));

    // Create a callback that sends chunks to the processor
    let processor = stream_processor.clone();
    let callback: StreamingCallback = Box::new(move |chunk: &StreamingChunk| {
        let mut processor = processor.lock().unwrap();
        processor.process(chunk)?;
        Ok(())
    });

    // Send a dummy message to trigger playback
    let _ = provider
        .send_message(LLMRequest::default(), Some(&callback))
        .await?;

    // Get the fragments from the UI to see what was displayed
    let fragments = test_ui.get_fragments();

    // Verify that fragments were created
    assert!(
        !fragments.is_empty(),
        "No fragments were created during playback"
    );

    // Print the fragments for debugging
    println!("Captured {} fragments:", fragments.len());
    for (i, fragment) in fragments.iter().enumerate() {
        println!("  Fragment {}: {:?}", i, fragment);
    }

    // Check for different fragment types to ensure we're processing correctly
    let has_plain_text = fragments
        .iter()
        .any(|f| matches!(f, crate::ui::DisplayFragment::PlainText(_)));
    let has_thinking = fragments
        .iter()
        .any(|f| matches!(f, crate::ui::DisplayFragment::ThinkingText(_)));
    let has_tool = fragments
        .iter()
        .any(|f| matches!(f, crate::ui::DisplayFragment::ToolName { .. }));

    // Assert only what we expect based on the specific recording
    // These assertions might need adjustment based on the actual content of assess-code-quality.json
    assert!(has_plain_text, "No plain text fragments found");

    // Print final summary
    println!("Test summary:");
    println!("  Has plain text: {}", has_plain_text);
    println!("  Has thinking text: {}", has_thinking);
    println!("  Has tool usage: {}", has_tool);

    Ok(())
}
