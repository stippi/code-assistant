//! A fired wakeup is no message from the user: it shows as the
//! `schedule_wakeup` block of its turn having fired.

use crate::test_support::{MainScreenTest, TestCore};
use agent_core::{SerializedToolExecution, ToolRequest};
use code_assistant_core::mocks::MockLLMProvider;
use code_assistant_core::session::lifecycle::SessionStatus;
use code_assistant_core::session::wakeup::ArmedWakeupInfo;
use code_assistant_core::session::wakeup::wakeup_message;
use code_assistant_core::ui::UiEvent;
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

fn metadata(session_id: &str) -> code_assistant_core::persistence::ChatMetadata {
    let now = std::time::SystemTime::now();
    code_assistant_core::persistence::ChatMetadata {
        id: session_id.to_string(),
        name: "s".into(),
        created_at: now,
        updated_at: now,
        message_count: 1,
        total_usage: Default::default(),
        last_usage: Default::default(),
        tokens_limit: None,
        tool_syntax: code_assistant_core::types::ToolSyntax::Native,
        initial_project: String::new(),
        branch: None,
        plan_collapsed: false,
        is_resumable: false,
    }
}

fn armed(test: &mut MainScreenTest, session_id: &str, wakeups: Vec<ArmedWakeupInfo>) {
    test.receive(
        session_id,
        UiEvent::UpdateArmedWakeups {
            session_id: session_id.to_string(),
            wakeups,
        },
    );
}

#[gpui_kit::test]
fn a_wakeup_to_fire_shows_in_the_sidebar_and_below_the_transcript(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.push(UiEvent::UpdateChatList {
        sessions: vec![metadata("viewed"), metadata("other")],
    });
    test.view_session("viewed");
    let status = |test: &mut MainScreenTest, id: &str| {
        test.main_screen.read_with(&test.cx, |screen, cx| {
            screen.project_sidebar.read(cx).session_status(id, cx)
        })
    };
    let shown = |test: &mut MainScreenTest| {
        let view = test.gpui.messages_view.lock().unwrap().clone().unwrap();
        test.cx.update(|_, cx| view.read(cx).armed_wakeups().len())
    };
    let wakeup = ArmedWakeupInfo {
        id: 1,
        fires_at: chrono::Utc::now() + chrono::Duration::minutes(2),
        prompt: "check the build".into(),
    };

    armed(&mut test, "other", vec![wakeup.clone()]);
    assert_eq!(status(&mut test, "other"), Some(SessionStatus::Scheduled));
    assert_eq!(status(&mut test, "viewed"), Some(SessionStatus::Ready));
    // Another session's wakeups don't show below this transcript.
    assert_eq!(shown(&mut test), 0);

    armed(&mut test, "viewed", vec![wakeup]);
    assert_eq!(status(&mut test, "viewed"), Some(SessionStatus::Scheduled));
    assert_eq!(shown(&mut test), 1);

    // Fired or cancelled.
    armed(&mut test, "viewed", Vec::new());
    assert_eq!(status(&mut test, "viewed"), Some(SessionStatus::Ready));
    assert_eq!(shown(&mut test), 0);
}
