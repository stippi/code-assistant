//! Showing a session whose large tool results are stored apart: they are
//! read when a block shows them.

use crate::test_support::{MainScreenTest, TestCore};
use agent_core::{SerializedToolExecution, ToolRequest};
use code_assistant_core::mocks::MockLLMProvider;
use code_assistant_core::ui::ui_events::ToolResultData;
use code_assistant_core::ui::{ToolStatus, UiEvent};
use gpui_kit::TestAppContext;
use llm::{ContentBlock, Message};

fn execution(id: &str, tool: &str, result_json: serde_json::Value) -> SerializedToolExecution {
    SerializedToolExecution {
        tool_request: ToolRequest {
            id: id.into(),
            name: tool.into(),
            input: serde_json::json!({}),
            start_offset: None,
            end_offset: None,
        },
        tool_name: tool.into(),
        result_json,
    }
}

/// A core with a session in which the assistant ran a command (shown as an
/// expanded card) and a glob (shown as a collapsed inline block), both with
/// outputs too large to come with the snapshot.
fn core_with_stored_outputs() -> (TestCore, String) {
    let core = TestCore::new(MockLLMProvider::new(Vec::new()).into_factory());
    let session_id = core.create_session();
    let output = "built ".repeat(2_000);
    let files: Vec<String> = (0..500).map(|i| format!("src/file_{i}.rs")).collect();
    let executions = vec![
        // A card, shown expanded.
        execution(
            "make",
            "execute_command",
            serde_json::json!({
                "project": "p", "command_line": "make", "working_dir": null,
                "output": output, "success": true,
            }),
        ),
        // An inline block, shown collapsed.
        execution(
            "glob",
            "glob_files",
            serde_json::json!({ "project": "p", "pattern": "**/*.rs", "files": files }),
        ),
    ];
    core.store()
        .update_entry(&session_id, |session| {
            session.add_message(Message::new_user("build it"));
            session.add_message(Message::new_assistant_content(
                executions
                    .iter()
                    .map(|exec| {
                        ContentBlock::new_tool_use(
                            exec.tool_request.id.clone(),
                            exec.tool_name.clone(),
                            serde_json::json!({}),
                        )
                    })
                    .collect(),
            ));
            session.add_message(Message::new_user_content(
                executions
                    .iter()
                    .map(|exec| ContentBlock::new_tool_result(exec.tool_request.id.clone(), ""))
                    .collect(),
            ));
            session.tool_executions = executions.clone();
            Ok(())
        })
        .unwrap();
    (core, session_id)
}

#[gpui_kit::test]
fn stored_outputs_load_when_their_blocks_show_them(cx: &mut TestAppContext) {
    let (core, session_id) = core_with_stored_outputs();
    let mut test = MainScreenTest::with_core(cx, core);

    test.gpui.cmd_load_session(session_id.clone(), None);
    test.wait_until("the card shows its output", |test| {
        test.tool_block("make")
            .and_then(|tool| tool.output)
            .is_some_and(|shown| shown.contains("built built"))
    });
    let glob = test.tool_block("glob").unwrap();
    assert!(glob.output_deferred);
    assert_eq!(glob.output, None);

    test.toggle_tool_block("glob");
    test.wait_until("the expanded block shows its output", |test| {
        test.tool_block("glob")
            .and_then(|tool| tool.output)
            .is_some_and(|shown| shown.contains("src/file_499.rs"))
    });
    assert!(!test.tool_block("glob").unwrap().output_deferred);
}

#[gpui_kit::test]
fn loaded_outputs_keep_a_following_transcript_at_the_bottom_without_animating(
    cx: &mut TestAppContext,
) {
    let (core, session_id) = core_with_stored_outputs();
    let mut test = MainScreenTest::with_core(cx, core);

    test.gpui.cmd_load_session(session_id, None);
    test.wait_until("the card shows its output", |test| {
        test.tool_block("make")
            .is_some_and(|tool| tool.output.is_some())
    });

    // The output was there all along; showing it is no new content to
    // scroll towards.
    let (following, animating) = test.scroll_state();
    assert!(following);
    assert!(!animating);
}

#[gpui_kit::test]
fn outputs_loaded_for_a_session_no_longer_shown_are_dropped(cx: &mut TestAppContext) {
    let (core, session_id) = core_with_stored_outputs();
    let mut test = MainScreenTest::with_core(cx, core);
    test.gpui.cmd_load_session(session_id.clone(), None);
    let id = session_id.clone();
    test.wait_until("the session is shown", |test| {
        test.gpui.get_current_session_id().as_deref() == Some(id.as_str())
            && test.tool_block("glob").is_some()
    });
    let glob = |shown: &str| ToolResultData {
        tool_id: "glob".into(),
        status: ToolStatus::Success,
        message: None,
        output: Some(shown.into()),
        styled_output: None,
        duration_seconds: None,
        images: Vec::new(),
        output_deferred: false,
    };

    // Asked for by a session shown before: tool IDs are only unique per
    // session.
    test.push(UiEvent::ToolOutputsLoaded {
        session_id: "earlier".into(),
        results: vec![glob("from another session")],
    });
    assert!(test.tool_block("glob").unwrap().output_deferred);

    test.push(UiEvent::ToolOutputsLoaded {
        session_id,
        results: vec![glob("src/main.rs")],
    });
    let shown = test.tool_block("glob").unwrap();
    assert!(!shown.output_deferred);
    assert_eq!(shown.output.as_deref(), Some("src/main.rs"));
}
