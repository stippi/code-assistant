//! The main screen in front of the real session core, with a scripted LLM.

use crate::test_support::{MainScreenTest, TestCore};
use code_assistant_core::mocks::{MockLLMProvider, create_test_response_text};
use gpui_kit::TestAppContext;

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
