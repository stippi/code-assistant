//! Renaming the viewed session from its name in the title bar.

use crate::test_support::{MainScreenTest, TestCore};
use code_assistant_core::mocks::MockLLMProvider;
use code_assistant_core::persistence::ChatSession;
use gpui_kit::TestAppContext;

fn screen(cx: &mut TestAppContext) -> MainScreenTest {
    let llm = MockLLMProvider::new(vec![]).streaming().into_factory();
    MainScreenTest::with_core(cx, TestCore::new(llm))
}

fn stored(test: &MainScreenTest, session_id: &str) -> ChatSession {
    let core = test.core.as_ref().expect("a test with a core");
    core.store()
        .load_chat_session(session_id)
        .unwrap()
        .expect("the session is stored")
}

#[gpui_kit::test]
fn enter_renames_the_session(cx: &mut TestAppContext) {
    let mut test = screen(cx);
    let session_id = test.open_new_session();

    let title = test.cx.debug_bounds("session-title").unwrap();
    test.click("session-title");
    // The field lies where the title was, as wide as its text.
    let field = test.cx.debug_bounds("session-title-input").unwrap();
    assert_eq!(field.origin, title.origin);
    assert_eq!(field.size.height, title.size.height);
    // The field opens with the old name selected; typing replaces it.
    test.cx.simulate_input("Chosen name");
    let grown = test.cx.debug_bounds("session-title-input").unwrap();
    assert!(grown.size.width > field.size.width);
    test.cx.simulate_keystrokes("enter");
    test.wait_until("the name is stored", |test| {
        stored(test, &session_id).name == "Chosen name"
    });

    assert!(stored(&test, &session_id).name_set_by_user);
    assert!(!test.shows("session-title-input"));
    assert!(test.shows("session-title"));
}

#[gpui_kit::test]
fn escape_keeps_the_old_name(cx: &mut TestAppContext) {
    let mut test = screen(cx);
    let session_id = test.open_new_session();

    test.click("session-title");
    test.cx.simulate_input("Discarded");
    test.cx.simulate_keystrokes("escape");
    test.settle();

    assert!(!test.shows("session-title-input"));
    let session = stored(&test, &session_id);
    assert_eq!(session.name, "");
    assert!(!session.name_set_by_user);
}
