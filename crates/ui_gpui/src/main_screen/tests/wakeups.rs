//! A fired wakeup is no message from the user: it shows as the
//! `schedule_wakeup` block of its turn having fired.

use crate::test_support::{MainScreenTest, TestCore};
use agent_core::{SerializedToolExecution, ToolRequest};
use code_assistant_core::mocks::MockLLMProvider;
use code_assistant_core::session::wakeup::wakeup_message;
use gpui_kit::TestAppContext;
use llm::{ContentBlock, Message};

/// A session whose agent scheduled a wakeup and ended its turn, then
/// `between` was said, then the wakeup fired.
fn core_with_fired_wakeup(between: &[Message]) -> (TestCore, String) {
    core_with_wakeup_message(between, &wakeup_message(1, "check the build"))
}

/// Like [`core_with_fired_wakeup`], with the wakeup message as `fired`.
fn core_with_wakeup_message(between: &[Message], fired: &str) -> (TestCore, String) {
    let core = TestCore::new(MockLLMProvider::new(Vec::new()).into_factory());
    let session_id = core.create_session();
    let input = serde_json::json!({ "delay_seconds": 60, "prompt": "check the build" });
    let execution = SerializedToolExecution {
        tool_request: ToolRequest {
            id: "wake".into(),
            name: "schedule_wakeup".into(),
            input: input.clone(),
            start_offset: None,
            end_offset: None,
        },
        tool_name: "schedule_wakeup".into(),
        result_json: serde_json::json!({
            "wakeup_id": 1, "delay_seconds": 60, "fires_at": "2026-10-12T12:00:00Z",
        }),
    };
    core.store()
        .update_entry(&session_id, |session| {
            session.add_message(Message::new_user("build it"));
            session.add_message(Message::new_assistant_content(vec![
                ContentBlock::new_tool_use("wake", "schedule_wakeup", input.clone()),
            ]));
            session.add_message(Message::new_user_content(vec![
                ContentBlock::new_tool_result("wake", ""),
            ]));
            session.add_message(Message::new_assistant("I'll check back."));
            for message in between {
                session.add_message(message.clone());
            }
            session.add_message(Message::new_user(fired));
            session.tool_executions = vec![execution.clone()];
            Ok(())
        })
        .unwrap();
    (core, session_id)
}

fn show(cx: &mut TestAppContext, core: TestCore, session_id: String) -> MainScreenTest {
    let mut test = MainScreenTest::with_core(cx, core);
    test.gpui.cmd_load_session(session_id, None);
    test.wait_until("the wakeup block is shown", |test| {
        test.tool_block("wake")
            .is_some_and(|tool| tool.output.is_some())
    });
    test
}

#[gpui_kit::test]
fn wakeup_of_the_same_turn_shows_on_its_block(cx: &mut TestAppContext) {
    let (core, session_id) = core_with_fired_wakeup(&[]);
    let mut test = show(cx, core, session_id);

    assert!(test.tool_block("wake").unwrap().wakeup_fired);
    assert_eq!(
        test.transcript(),
        vec![
            "user: build it",
            "assistant: ",
            "assistant: I'll check back."
        ]
    );
}

#[gpui_kit::test]
fn wakeup_after_the_user_spoke_shows_on_its_own(cx: &mut TestAppContext) {
    let (core, session_id) = core_with_fired_wakeup(&[
        Message::new_user("anything else?"),
        Message::new_assistant("No."),
    ]);
    let mut test = show(cx, core, session_id);

    assert!(!test.tool_block("wake").unwrap().wakeup_fired);
    assert_eq!(
        test.transcript().last().map(String::as_str),
        Some("wakeup: check the build")
    );
}

#[gpui_kit::test]
fn wakeup_message_without_id_is_linked_by_its_prompt(cx: &mut TestAppContext) {
    let (core, session_id) = core_with_wakeup_message(&[], "[scheduled wakeup] check the build");
    let mut test = show(cx, core, session_id);

    assert!(test.tool_block("wake").unwrap().wakeup_fired);
    assert_eq!(test.transcript().len(), 3);
}

#[gpui_kit::test]
fn wakeup_message_of_another_id_is_not_linked(cx: &mut TestAppContext) {
    let (core, session_id) = core_with_wakeup_message(&[], &wakeup_message(2, "check the build"));
    let mut test = show(cx, core, session_id);

    assert!(!test.tool_block("wake").unwrap().wakeup_fired);
    assert_eq!(
        test.transcript().last().map(String::as_str),
        Some("wakeup: check the build")
    );
}
