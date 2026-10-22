//! Running sub-agents show below the transcript; background results show as
//! the sub-agent's line, not as a message from the user.

use crate::test_support::{MainScreenTest, TestCore};
use code_assistant_core::agent::sub_agent::{
    FinishedOutcome, FinishedSubAgent, RunningSubAgentInfo, SubAgentActivity,
};
use code_assistant_core::mocks::MockLLMProvider;
use code_assistant_core::ui::UiEvent;
use gpui_kit::TestAppContext;
use llm::{ContentBlock, Message};

fn running(id: u64, tool_id: &str, background: bool) -> RunningSubAgentInfo {
    RunningSubAgentInfo {
        id,
        tool_id: tool_id.into(),
        title: format!("Task {id}"),
        background,
        started_at: chrono::Utc::now(),
        activity: Some(SubAgentActivity::WaitingForLlm),
        tool_count: 0,
        current_tool: None,
        session_id: None,
    }
}

fn update(test: &mut MainScreenTest, session_id: &str, agents: Vec<RunningSubAgentInfo>) {
    test.receive(
        session_id,
        UiEvent::UpdateRunningSubAgents {
            session_id: session_id.to_string(),
            agents,
        },
    );
}

fn shown(test: &mut MainScreenTest) -> Vec<u64> {
    let view = test.gpui.messages_view.lock().unwrap().clone().unwrap();
    test.cx.update(|_, cx| {
        view.read(cx)
            .running_sub_agents()
            .iter()
            .map(|a| a.id)
            .collect()
    })
}

#[gpui_kit::test]
fn running_sub_agents_of_the_viewed_session_show_below_the_transcript(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.view_session("viewed");

    update(
        &mut test,
        "viewed",
        vec![running(1, "t1", true), running(2, "t2", false)],
    );
    assert_eq!(shown(&mut test), vec![1, 2]);

    // Another session's sub-agents don't show here.
    update(&mut test, "other", vec![running(7, "t7", true)]);
    assert_eq!(shown(&mut test), vec![1, 2]);

    update(&mut test, "viewed", vec![running(2, "t2", false)]);
    assert_eq!(shown(&mut test), vec![2]);
    update(&mut test, "viewed", Vec::new());
    assert!(shown(&mut test).is_empty());
}

#[gpui_kit::test]
fn delivered_results_show_as_the_sub_agents_line(cx: &mut TestAppContext) {
    let core = TestCore::new(MockLLMProvider::new(Vec::new()).into_factory());
    let session_id = core.create_session();
    let result = |agent_id, outcome| {
        FinishedSubAgent {
            agent_id,
            tool_id: format!("t{agent_id}"),
            outcome,
            ui_output: String::new(),
        }
        .message()
    };
    core.store()
        .update_entry(&session_id, |session| {
            session.add_message(Message::new_user("look around"));
            session.add_message(Message::new_assistant("Started two sub-agents."));
            session.add_message(Message::new_user_content(vec![
                ContentBlock::new_text(result(
                    1,
                    FinishedOutcome::Completed {
                        answer: "It is in a.rs".into(),
                    },
                )),
                ContentBlock::new_text(result(
                    2,
                    FinishedOutcome::Failed {
                        error: "boom".into(),
                    },
                )),
            ]));
            Ok(())
        })
        .unwrap();

    let mut test = MainScreenTest::with_core(cx, core);
    test.gpui.cmd_load_session(session_id, None);
    test.wait_until("the transcript is shown", |test| {
        test.transcript().len() == 3
    });
    assert_eq!(
        test.transcript().last().map(String::as_str),
        Some("sub-agent #1 finished: It is in a.rs | sub-agent #2 failed: boom")
    );
}

#[gpui_kit::test]
fn a_sub_agent_transcript_opens_read_only(cx: &mut TestAppContext) {
    let core = TestCore::new(MockLLMProvider::new(Vec::new()).into_factory());
    let parent = core.create_session();
    let transcript_id = code_assistant_core::persistence::layout::sub_agent_session_id(&parent, 1);
    let mut transcript = code_assistant_core::persistence::ChatSession::new_empty(
        transcript_id.clone(),
        "Sub-agent #1: look".into(),
        Default::default(),
        None,
    );
    transcript.add_message(Message::new_user("Find the parser"));
    transcript.add_message(Message::new_assistant("It is in parser.rs"));
    core.store().create_chat_session(&transcript).unwrap();

    let mut test = MainScreenTest::with_core(cx, core);
    test.gpui.cmd_load_session(transcript_id, None);
    test.wait_until("the transcript is shown", |test| {
        test.transcript().len() == 2
    });
    assert_eq!(
        test.transcript(),
        vec!["user: Find the parser", "assistant: It is in parser.rs"]
    );
    let input_area = test
        .main_screen
        .read_with(&test.cx, |screen, _| screen.input_area.clone());
    let marked = test
        .cx
        .update(|_, cx| input_area.read(cx).sub_agent_transcript().cloned());
    assert_eq!(marked, Some((parent, 1)));
}
