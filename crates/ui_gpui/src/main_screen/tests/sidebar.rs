//! The session sidebar as the main screen feeds it.

use crate::test_support::MainScreenTest;
use code_assistant_core::ui::UiEvent;
use gpui_kit::TestAppContext;

#[gpui_kit::test]
fn saved_projects_show_before_the_first_session_exists(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    // Replace what the machine's projects.json holds.
    *test.gpui.persisted_projects.lock().unwrap() = ["proj".to_string()].into();

    // A fresh store: the listing arrives, and it is empty.
    test.push(UiEvent::UpdateChatList {
        sessions: Vec::new(),
    });

    let folders = test.main_screen.read_with(&test.cx, |screen, cx| {
        screen.project_sidebar.read(cx).folder_projects()
    });
    assert_eq!(folders, vec![Some("proj".to_string())]);
}
