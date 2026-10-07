//! Behaviour of the main screen, driven through [`MainScreenTest`].

use crate::test_support::MainScreenTest;
use code_assistant_core::ui::UiEvent;
use gpui_kit::TestAppContext;
use std::io::ErrorKind;

fn handoff(prompt: &str) -> UiEvent {
    UiEvent::HandoffPrepared {
        prompt: prompt.into(),
    }
}

fn edit_ready(content: &str, branch_parent_id: u64) -> UiEvent {
    UiEvent::MessageEditReady {
        content: content.into(),
        attachments: Vec::new(),
        branch_parent_id: Some(branch_parent_id),
        messages: Vec::new(),
        tool_results: Vec::new(),
    }
}

// --- Drafts ---------------------------------------------------------------

#[gpui_kit::test]
fn typed_text_is_kept_per_session(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.view_session("a");
    test.type_text("for a");
    test.view_session("b");
    assert_eq!(test.input_text(), "");

    test.type_text("for b");
    test.view_session("a");
    assert_eq!(test.input_text(), "for a");
    assert_eq!(test.stores.drafts.stored("b").unwrap().message, "for b");
}

#[gpui_kit::test]
fn a_draft_stays_while_its_writes_fail(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.stores.drafts.fail_writes(Some(ErrorKind::StorageFull));
    test.view_session("a");
    test.type_text("unsaved");
    test.view_session("b");
    test.view_session("a");

    assert_eq!(test.input_text(), "unsaved");
    assert!(test.stores.drafts.stored("a").is_none());
}

#[gpui_kit::test]
fn a_draft_stays_while_its_reads_fail(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.view_session("a");
    test.type_text("typed before");
    test.stores
        .drafts
        .fail_reads(Some(ErrorKind::PermissionDenied));
    test.view_session("b");
    test.view_session("a");

    assert_eq!(test.input_text(), "typed before");
}

// --- Prepared handoffs ----------------------------------------------------

#[gpui_kit::test]
fn a_handoff_offered_in_the_viewed_session_survives_a_switch(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.view_session("a");
    test.receive("a", handoff("Next step"));
    assert_eq!(test.input_text(), "/new Next step");

    test.view_session("b");
    test.view_session("a");
    assert_eq!(test.input_text(), "/new Next step");
}

#[gpui_kit::test]
fn a_handoff_does_not_replace_typed_text(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.view_session("a");
    test.type_text("my own words");
    test.receive("a", handoff("Next step"));

    assert_eq!(test.input_text(), "my own words");
    assert_eq!(
        test.stores.drafts.stored("a").unwrap().message,
        "my own words"
    );
}

#[gpui_kit::test]
fn a_handoff_prepared_in_the_background_waits_in_its_draft(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.view_session("a");
    test.receive("b", handoff("Next step"));
    assert_eq!(test.input_text(), "");
    assert_eq!(
        test.stores.drafts.stored("b").unwrap().message,
        "/new Next step"
    );

    test.view_session("b");
    assert_eq!(test.input_text(), "/new Next step");
}

#[gpui_kit::test]
fn a_handoff_prepared_in_the_background_keeps_its_draft(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.view_session("b");
    test.type_text("left here");
    test.view_session("a");
    test.receive("b", handoff("Next step"));

    test.view_session("b");
    assert_eq!(test.input_text(), "left here");
}

// --- Message edits --------------------------------------------------------

#[gpui_kit::test]
fn a_started_edit_survives_a_switch(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.view_session("a");
    test.push(edit_ready("original message", 7));
    assert!(test.is_editing());
    assert_eq!(test.input_text(), "original message");

    test.view_session("b");
    assert!(!test.is_editing(), "the edit banner stays with its session");

    test.view_session("a");
    assert!(test.is_editing());
    assert_eq!(test.input_text(), "original message");
    let stored = test.stores.drafts.stored("a").unwrap();
    assert_eq!(stored.editing_branch_parent_id, Some(7));
}
