//! Monitors and finished background jobs: a running monitor keeps its
//! session scheduled, and their notifications are no messages from the user.

use crate::test_support::{MainScreenTest, TestCore};
use code_assistant_core::mocks::MockLLMProvider;
use code_assistant_core::session::lifecycle::SessionStatus;
use code_assistant_core::session::monitor::{
    ActiveMonitorInfo, job_exited_message, monitor_event_message,
};
use code_assistant_core::ui::UiEvent;
use gpui_kit::TestAppContext;
use llm::Message;

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

fn active(test: &mut MainScreenTest, session_id: &str, monitors: Vec<ActiveMonitorInfo>) {
    test.receive(
        session_id,
        UiEvent::UpdateActiveMonitors {
            session_id: session_id.to_string(),
            monitors,
        },
    );
}

#[gpui_kit::test]
fn a_running_monitor_shows_in_the_sidebar_and_below_the_transcript(cx: &mut TestAppContext) {
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
        test.cx
            .update(|_, cx| view.read(cx).active_monitors().len())
    };
    let now = chrono::Utc::now();
    let monitor = ActiveMonitorInfo {
        id: 1,
        description: "CI of PR #42".into(),
        command_line: "watch-ci 42".into(),
        started_at: now,
        ends_at: now + chrono::Duration::hours(1),
    };

    active(&mut test, "other", vec![monitor.clone()]);
    assert_eq!(status(&mut test, "other"), Some(SessionStatus::Scheduled));
    assert_eq!(status(&mut test, "viewed"), Some(SessionStatus::Ready));
    // Another session's monitors don't show below this transcript.
    assert_eq!(shown(&mut test), 0);

    active(&mut test, "viewed", vec![monitor]);
    assert_eq!(status(&mut test, "viewed"), Some(SessionStatus::Scheduled));
    assert_eq!(shown(&mut test), 1);

    // Ended or stopped.
    active(&mut test, "viewed", Vec::new());
    assert_eq!(status(&mut test, "viewed"), Some(SessionStatus::Ready));
    assert_eq!(shown(&mut test), 0);
}

#[gpui_kit::test]
fn notifications_show_as_notifications(cx: &mut TestAppContext) {
    let core = TestCore::new(MockLLMProvider::new(Vec::new()).into_factory());
    let session_id = core.create_session();
    let event = monitor_event_message(1, "CI of PR #42", &["checks: 3 pass".into()], 0);
    let job = job_exited_message(12345, "cargo build", Some(0));
    core.store()
        .update_entry(&session_id, |session| {
            session.add_message(Message::new_user("watch the PR"));
            session.add_message(Message::new_assistant("Watching."));
            session.add_message(Message::new_user(&event));
            session.add_message(Message::new_assistant("Checks pass."));
            // Queued while a turn ran: several in one message.
            session.add_message(Message::new_user(format!("{event}\n\n{job}")));
            session.add_message(Message::new_user("thanks"));
            Ok(())
        })
        .unwrap();

    let mut test = MainScreenTest::with_core(cx, core);
    test.gpui.cmd_load_session(session_id, None);
    test.wait_until("the transcript is shown", |test| {
        test.transcript().len() >= 5
    });
    assert_eq!(
        test.transcript(),
        vec![
            "user: watch the PR",
            "assistant: Watching.",
            "notification: Monitor #1 · CI of PR #42",
            "assistant: Checks pass.",
            "notification: Monitor #1 · CI of PR #42 | Background job finished · cargo build",
            "user: thanks",
        ]
    );
}
