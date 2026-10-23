//! The Architecture view of the right panel, in front of the real core: the
//! project's components from the scan, explained by a scripted model.

use crate::main_screen::right_panel::RightPanelView;
use crate::main_screen::right_panel::architecture::ArchitectureView;
use crate::test_support::{MainScreenTest, TestCore};
use code_assistant_core::line_comments::LineComment;
use code_assistant_core::mocks::{MockLLMProvider, create_test_response_text};
use code_assistant_core::session::SessionConfig;
use gpui_kit::{Entity, TestAppContext};
use std::path::Path;

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, text).unwrap();
}

impl MainScreenTest {
    fn architecture(&mut self) -> Entity<ArchitectureView> {
        let panel = self
            .main_screen
            .read_with(&self.cx, |s, _| s.right_panel.clone());
        panel.read_with(&self.cx, |panel, _| panel.architecture().clone())
    }

    fn with_architecture<T>(&mut self, f: impl FnOnce(&ArchitectureView) -> T) -> T {
        let view = self.architecture();
        view.read_with(&self.cx, |view, _| f(view))
    }

    fn draft_comments(&mut self) -> Vec<LineComment> {
        let input = self
            .main_screen
            .read_with(&self.cx, |s, _| s.input_area.clone());
        input.read_with(&self.cx, |input, _| input.comments().to_vec())
    }

    fn hover(&mut self, selector: &'static str) {
        let bounds = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("nothing rendered as {selector}"));
        self.cx
            .simulate_mouse_move(bounds.center(), None, gpui_kit::Modifiers::none());
        self.settle();
    }
}

#[gpui_kit::test]
fn the_architecture_view_shows_explains_and_drills_into_components(cx: &mut TestAppContext) {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(
        root,
        "crates/core/Cargo.toml",
        "[package]\nname = \"demo-core\"\n",
    );
    write(
        root,
        "crates/core/src/lib.rs",
        "//! The domain.\npub mod model;\npub mod store;\n",
    );
    write(root, "crates/core/src/model.rs", "pub struct Order;\n");
    write(
        root,
        "crates/core/src/store.rs",
        "use crate::model::Order;\npub fn save(_: Order) {}\n",
    );
    write(root, "crates/app/Cargo.toml", "[package]\nname = \"app\"\n");
    write(
        root,
        "crates/app/src/main.rs",
        "use demo_core::model::Order;\n",
    );

    // Answers are taken from the end: the project first, then `core`.
    let llm = MockLLMProvider::new(vec![
        Ok(create_test_response_text(
            r#"{"summary": "The domain crate.", "constraints": ["store depends on model"],
                "parts": {"model.rs": "Domain types", "store.rs": "Persists orders"}}"#,
        )),
        Ok(create_test_response_text(
            r#"```json
{"summary": "A demo shop.", "responsibility": "The app drives the core.",
 "constraints": ["app depends on core, never the reverse"],
 "interfaces": [{"name": "Order", "description": "the domain type"}],
 "parts": {"app": "Runs the shop", "core": "Holds the domain"}}
```"#,
        )),
    ]);
    let core = TestCore::new(llm.into_factory());
    let config = SessionConfig {
        init_path: Some(root.to_path_buf()),
        ..SessionConfig::default()
    };
    let session_id = core
        .block_on(core.service.create_session_with_config(None, config, None))
        .unwrap();
    let mut test = MainScreenTest::with_core(cx, core);
    test.gpui.cmd_load_session(session_id.clone(), None);
    let id = session_id.clone();
    test.wait_until("the session is shown", |test| {
        test.gpui.get_current_session_id().as_deref() == Some(id.as_str())
    });

    test.click("right-panel-view-Architecture");
    test.wait_until("the project is explained", |test| {
        test.with_architecture(|view| view.summary("").is_some())
    });
    test.with_architecture(|view| {
        assert_eq!(view.summary("").unwrap().summary, "A demo shop.");
        assert_eq!(
            view.shown_parts(),
            [
                ("app".to_owned(), "Runs the shop".to_owned()),
                ("core".to_owned(), "Holds the domain".to_owned()),
            ]
        );
    });
    assert!(test.shows("architecture-box-core"));

    // A box takes a comment, sent with what the overview says about it.
    assert!(!test.shows("architecture-comment-core"));
    test.hover("architecture-box-core");
    test.click("architecture-comment-core");
    test.cx.simulate_input("keep the domain free of IO");
    test.click("comment-save");
    let comments = test.draft_comments();
    assert_eq!(comments.len(), 1);
    let comment = &comments[0];
    assert!(comment.on_component);
    assert_eq!(comment.file, root.join("crates/core"));
    assert_eq!(comment.text, "keep the domain free of IO");
    assert!(
        comment
            .excerpt
            .starts_with("core (package) at crates/core\nHolds the domain")
    );
    assert!(
        comment.excerpt.contains("Used by: app"),
        "{}",
        comment.excerpt
    );
    // Marked even when not hovered.
    test.hover("architecture-box-app");
    assert!(test.shows("architecture-comment-core"));

    // A package opens its level, explained by a second call.
    test.click("architecture-box-core");
    test.wait_until("core is explained", |test| {
        test.with_architecture(|view| view.summary("crates/core").is_some())
    });
    test.with_architecture(|view| {
        assert_eq!(view.current(), "crates/core");
        assert_eq!(
            view.shown_parts(),
            [
                ("model.rs".to_owned(), "Domain types".to_owned()),
                ("store.rs".to_owned(), "Persists orders".to_owned()),
            ]
        );
    });

    // The level's own card edits the same comment.
    test.click("architecture-comment-current");
    test.cx.simulate_input(" — or is it too small?");
    test.click("comment-save");
    let comments = test.draft_comments();
    assert_eq!(comments.len(), 1);
    assert!(comments[0].text.contains("or is it too small?"));

    // A file opens in the Files view.
    test.click("architecture-box-model.rs");
    let panel = test
        .main_screen
        .read_with(&test.cx, |s, _| s.right_panel.clone());
    panel.read_with(&test.cx, |panel, cx| {
        assert_eq!(panel.active_view(), RightPanelView::Files);
        assert_eq!(
            panel.files_open_path(cx).as_deref(),
            Some("crates/core/src/model.rs")
        );
    });

    // Revealing the comment from the composer shows the component and
    // opens the comment.
    let comment = test.draft_comments()[0].clone();
    panel.update(&mut test.cx, |panel, cx| panel.reveal_comment(comment, cx));
    test.wait_until("the comment is open again", |test| {
        test.with_architecture(|view| view.comment_editor_open())
    });
    test.with_architecture(|view| assert_eq!(view.current(), ""));
}
