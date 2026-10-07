//! The main screen in front of the real session core, with a scripted LLM.

use crate::test_support::{MainScreenTest, TestCore};
use code_assistant_core::mocks::{
    MockLLMProvider, PendingLLMProvider, create_test_response, create_test_response_text,
};
use gpui_kit::TestAppContext;
use tools_core::PermissionTier;

/// Agent runs answering with `replies`, one per run, in order.
fn replying(replies: &[&str]) -> TestCore {
    let responses = replies
        .iter()
        .rev()
        .map(|text| Ok(create_test_response_text(text)))
        .collect();
    TestCore::new(MockLLMProvider::new(responses).streaming().into_factory())
}

#[gpui_kit::test]
fn a_sent_message_gets_the_agents_reply(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::with_core(cx, replying(&["Hello back"]));
    let session_id = test.open_new_session();

    test.send("Hello");
    test.wait_until("the agent replied and stopped", |test| {
        !test.agent_is_running() && test.transcript().len() == 2
    });

    assert_eq!(test.transcript(), ["user: Hello", "assistant: Hello back"]);
    assert_eq!(test.input_text(), "");
    assert!(test.stores.drafts.stored(&session_id).is_none());
}

/// Agent runs that wait on the model until stopped.
fn waiting_for_the_model() -> TestCore {
    TestCore::new(PendingLLMProvider::default().into_factory())
}

#[gpui_kit::test]
fn a_message_sent_while_the_agent_works_waits_for_it(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::with_core(cx, waiting_for_the_model());
    test.open_new_session();
    test.send("first");
    test.wait_until("the agent works", |test| test.agent_is_running());

    test.send("second");
    test.wait_until("the message waits", |test| test.pending_message().is_some());
    assert_eq!(test.pending_message().as_deref(), Some("second"));
    assert_eq!(test.input_text(), "");
    assert_eq!(test.transcript(), ["user: first"]);
}

#[gpui_kit::test]
fn escape_stops_the_working_agent(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::with_core(cx, waiting_for_the_model());
    test.open_new_session();
    test.send("work on this");
    test.wait_until("the agent works", |test| test.agent_is_running());

    test.cx.simulate_keystrokes("escape");
    test.wait_until("the agent stopped", |test| !test.agent_is_running());
}

#[gpui_kit::test]
fn a_tool_waits_for_the_users_permission(cx: &mut TestAppContext) {
    let tool_call = create_test_response(
        "tool-1",
        "delete_files",
        serde_json::json!({ "project": "test", "paths": ["notes.txt"] }),
        "Deleting the notes.",
    );
    let llm = MockLLMProvider::new(vec![Ok(create_test_response_text("Done")), Ok(tool_call)]);
    let mut test = MainScreenTest::with_core(cx, TestCore::new(llm.streaming().into_factory()));
    let session_id = test.open_new_session();
    let core = test.core.as_ref().unwrap();
    core.block_on(
        core.service
            .change_permission_tier(session_id.clone(), PermissionTier::WriteTools),
    )
    .unwrap();

    test.send("Delete the notes");
    test.wait_until("the permission is asked", |test| {
        test.shows("permission-option-0")
    });
    assert!(test.agent_is_running(), "the agent waits for the answer");

    test.click("permission-option-0");
    test.wait_until("the agent finished", |test| {
        !test.agent_is_running()
            && test.transcript().last().map(String::as_str) == Some("assistant: Done")
    });
    assert!(!test.shows("permission-option-0"));
    assert!(
        test.gpui
            .get_pending_permission_requests(&session_id)
            .is_empty()
    );
}
