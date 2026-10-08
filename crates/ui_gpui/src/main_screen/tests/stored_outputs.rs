//! Showing a session whose large tool results are stored apart: they are
//! read when a block shows them.

use crate::test_support::{MainScreenTest, TestCore};
use agent_core::{SerializedToolExecution, ToolRequest};
use code_assistant_core::mocks::MockLLMProvider;
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

#[gpui_kit::test]
fn stored_outputs_load_when_their_blocks_show_them(cx: &mut TestAppContext) {
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
