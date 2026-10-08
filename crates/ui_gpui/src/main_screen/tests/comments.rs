//! Line comments in the composer, and the right panel: file requests and
//! the title bar switch.

use crate::main_screen::right_panel::{CommentChange, RightPanelView};
use crate::shared::open_file::{OpenFileRequest, request};
use crate::test_support::{MainScreenTest, TestCore};
use code_assistant_core::line_comments::LineComment;
use code_assistant_core::mocks::{MockLLMProvider, create_test_response_text};
use code_assistant_core::persistence::DraftAttachment;
use gpui_kit::TestAppContext;
use std::path::PathBuf;

fn comment(file: &str, lines: (usize, usize), text: &str) -> LineComment {
    LineComment {
        id: 0,
        file: PathBuf::from(file),
        start_line: lines.0,
        end_line: lines.1,
        old_side: false,
        in_diff: false,
        on_message: false,
        excerpt: "let x = y.unwrap();".into(),
        text: text.into(),
    }
}

impl MainScreenTest {
    fn add_comment(&mut self, comment: LineComment) {
        let panel = self
            .main_screen
            .read_with(&self.cx, |s, _| s.right_panel.clone());
        panel.update(&mut self.cx, |_, cx| {
            cx.emit(CommentChange::Upsert(comment))
        });
        self.settle();
    }

    fn comments(&mut self) -> Vec<LineComment> {
        let input = self
            .main_screen
            .read_with(&self.cx, |s, _| s.input_area.clone());
        input.read_with(&self.cx, |input, _| input.comments().to_vec())
    }
}

#[gpui_kit::test]
fn comments_from_the_panel_land_in_the_draft(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.view_session("a");
    test.add_comment(comment("/p/src/a.rs", (3, 4), "no unwrap"));
    test.add_comment(comment("/p/src/b.rs", (9, 9), "rename"));

    let comments = test.comments();
    assert_eq!(comments.len(), 2);
    assert_eq!((comments[0].id, comments[1].id), (1, 2));
    let draft = test.stores.drafts.stored("a").expect("draft saved");
    assert!(matches!(
        draft.attachments.as_slice(),
        [DraftAttachment::LineComments { comments }] if comments.len() == 2
    ));

    // Editing keeps the id; removing the last one drops the attachment.
    let mut edited = comments[0].clone();
    edited.text = "use ?".into();
    test.add_comment(edited);
    assert_eq!(test.comments()[0].text, "use ?");
    let panel = test
        .main_screen
        .read_with(&test.cx, |s, _| s.right_panel.clone());
    for id in [1, 2] {
        panel.update(&mut test.cx, |_, cx| cx.emit(CommentChange::Remove(id)));
    }
    test.settle();
    assert!(test.comments().is_empty());
}

#[gpui_kit::test]
fn comments_survive_switching_sessions(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.view_session("a");
    test.add_comment(comment("/p/src/a.rs", (3, 4), "no unwrap"));
    test.view_session("b");
    assert!(test.comments().is_empty());
    test.view_session("a");
    assert_eq!(test.comments().len(), 1);
}

#[gpui_kit::test]
fn comments_go_with_the_next_message(cx: &mut TestAppContext) {
    let core = TestCore::new(
        MockLLMProvider::new(vec![Ok(create_test_response_text("ok"))])
            .streaming()
            .into_factory(),
    );
    let mut test = MainScreenTest::with_core(cx, core);
    test.open_new_session();
    test.add_comment(comment("/p/src/a.rs", (3, 4), "no unwrap"));

    test.send("please fix");
    test.wait_until("the agent replied", |test| {
        !test.agent_is_running() && test.transcript().len() == 2
    });

    let transcript = test.transcript();
    assert!(transcript[0].starts_with("user: please fix"));
    assert!(
        transcript[0].contains("<comment path=\"/p/src/a.rs\" lines=\"3-4\">"),
        "{transcript:?}"
    );
    assert!(test.comments().is_empty());
}

#[gpui_kit::test]
fn an_open_file_request_shows_the_file_in_the_panel(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.view_session("a");
    test.cx.update(|_, cx| {
        request(
            OpenFileRequest {
                path: "./src/lib.rs".into(),
                line: Some(3),
                project: None,
            },
            cx,
        )
    });
    test.settle();

    let (collapsed, panel) = test.main_screen.read_with(&test.cx, |s, _| {
        (s.right_sidebar_collapsed, s.right_panel.clone())
    });
    assert!(!collapsed, "the panel opened");
    panel.read_with(&test.cx, |panel, cx| {
        assert_eq!(panel.active_view(), RightPanelView::Files);
        assert_eq!(panel.files_open_path(cx).as_deref(), Some("src/lib.rs"));
    });
}

fn panel_state(test: &mut MainScreenTest) -> Option<RightPanelView> {
    let (collapsed, panel) = test.main_screen.read_with(&test.cx, |s, _| {
        (s.right_sidebar_collapsed, s.right_panel.clone())
    });
    (!collapsed).then(|| panel.read_with(&test.cx, |panel, _| panel.active_view()))
}

#[gpui_kit::test]
fn the_title_bar_switch_opens_switches_and_closes_the_panel(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.view_session("a");
    assert_eq!(panel_state(&mut test), None);

    test.click("right-panel-view-Files");
    assert_eq!(panel_state(&mut test), Some(RightPanelView::Files));
    test.click("right-panel-view-Review");
    assert_eq!(panel_state(&mut test), Some(RightPanelView::Review));
    test.click("right-panel-view-Review");
    assert_eq!(panel_state(&mut test), None);
}

#[gpui_kit::test]
fn a_comment_on_a_message_reaches_the_draft_and_the_transcript(cx: &mut TestAppContext) {
    let mut test = MainScreenTest::new(cx);
    test.view_session("a");
    let mut on_message = LineComment::on_message("Use a ledger first.");
    on_message.text = "why not both?".into();
    test.cx.update(|_, cx| {
        crate::comments::report(crate::comments::CommentChange::Upsert(on_message), cx)
    });
    test.settle();

    let comments = test.comments();
    assert_eq!(comments.len(), 1);
    assert!(comments[0].on_message);
    // Message blocks see it through the global, to mark the quoted message.
    test.cx.update(|_, cx| {
        let marked = crate::comments::message_comments(cx);
        assert_eq!(marked.len(), 1);
        assert_eq!(marked[0].excerpt, "Use a ledger first.");
    });
}
