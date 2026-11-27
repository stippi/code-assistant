//! The Review tab in a window, fed listings and diffs as the core would.

use super::*;
use crate::comments::{CommentBus, CommentBusGlobal, CommentChange};
use crate::main_screen::right_panel::code_view::list::CodeHost;
use crate::shared::file_icons;
use code_assistant_core::session::ReviewScanState;
use git::{ChangeStatus, FileDiffContent};
use gpui_kit::{Modifiers, TestAppContext, VisualTestContext};
use std::cell::RefCell;
use std::rc::Rc;

fn listed(path: &str, status: ChangeStatus, fingerprint: &str) -> ChangedFile {
    ChangedFile {
        path: path.into(),
        orig_path: None,
        status,
        fingerprint: Some(fingerprint.into()),
    }
}

fn repo(root: &str, files: Vec<ChangedFile>) -> RepoReviewData {
    RepoReviewData {
        repo_root: PathBuf::from(root),
        label: root.rsplit('/').next().unwrap_or(root).to_owned(),
        current_branch: Some("main".into()),
        base_candidates: Vec::new(),
        base: None,
        files,
        stats: git::DiffStats::default(),
        scan_state: ReviewScanState::Done,
    }
}

fn view(cx: &mut TestAppContext) -> (Entity<ReviewView>, &mut VisualTestContext) {
    let window = cx.update(|cx| {
        gpui_kit::component::init(cx);
        file_icons::init(cx);
        cx.open_window(Default::default(), |window, cx| {
            cx.new(|cx| ReviewView::new(window, cx))
        })
        .unwrap()
    });
    let view = window.root(cx).unwrap();
    let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
    (view, cx)
}

fn list(view: &Entity<ReviewView>, repos: Vec<RepoReviewData>, cx: &mut VisualTestContext) {
    view.update_in(cx, |view, window, cx| {
        view.take_listing(
            crate::ReviewData {
                repos,
                is_git_repo: true,
                mode: ReviewMode::WorkingTree,
            },
            window,
            cx,
        )
    });
    cx.run_until_parked();
}

/// `lines` lines, `changed` of them (from the 10th on) edited.
fn text(lines: usize, changed: bool) -> String {
    (0..lines)
        .map(|i| match changed && (10..13).contains(&i) {
            true => format!("changed {i}\n"),
            false => format!("line {i}\n"),
        })
        .collect()
}

/// The diff of `path` arrives as read for its listing entry.
fn arrive(
    view: &Entity<ReviewView>,
    path: &str,
    old: Option<&str>,
    new: Option<&str>,
    cx: &mut VisualTestContext,
) {
    view.update(cx, |view, cx| {
        let listed = view.review_file(path).expect("listed").listed.clone();
        let content = FileDiffContent {
            old_text: old.map(str::to_owned),
            new_text: new.map(str::to_owned),
            is_binary: false,
            too_large: false,
        };
        let theme = cx.theme().highlight_theme.clone();
        let file = LoadedFile::from_content(&listed, &content, &theme, view.list.diff_view());
        let token = crate::shared::reviewed_files_store::content_token(&content);
        let scope = view.scope;
        view.diff_read(scope, Ok((listed, file, token)), cx);
    });
    cx.run_until_parked();
}

fn rows(view: &Entity<ReviewView>, cx: &mut VisualTestContext) -> Vec<CodeRow> {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert_eq!(view.list.list_state.item_count(), view.list.rows.len());
        view.list.rows.clone()
    })
}

fn click(cx: &mut VisualTestContext, selector: &'static str, modifiers: Modifiers) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("nothing rendered as {selector}"));
    cx.simulate_click(bounds.center(), modifiers);
    cx.run_until_parked();
}

#[gpui_kit::test]
fn files_show_headers_until_their_diffs_arrive(cx: &mut TestAppContext) {
    let (view, cx) = view(cx);
    list(
        &view,
        vec![repo(
            "/work",
            vec![
                listed("src/b.rs", ChangeStatus::Modified, "f1"),
                listed("src/a.rs", ChangeStatus::Added, "f1"),
            ],
        )],
        cx,
    );
    // Ordered like a file tree; headers only.
    let paths = view.read_with(cx, |view, _| {
        (0..view.shown_count())
            .map(|ix| view.shown_file(ix).path.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(paths, ["src/a.rs", "src/b.rs"]);
    assert_eq!(
        rows(&view, cx),
        [CodeRow::File { file: 0 }, CodeRow::File { file: 1 }]
    );

    arrive(&view, "src/a.rs", None, Some(&text(3, false)), cx);
    let rows = rows(&view, cx);
    assert_eq!(rows[0], CodeRow::File { file: 0 });
    assert!(
        matches!(rows[1], CodeRow::Chunk { file: 0, .. }),
        "{rows:?}"
    );
    assert_eq!(rows.last(), Some(&CodeRow::File { file: 1 }));
    assert!(cx.debug_bounds("review-counts").is_some());
}

#[gpui_kit::test]
fn unchanged_lines_reveal_from_the_text_already_read(cx: &mut TestAppContext) {
    let (view, cx) = view(cx);
    list(
        &view,
        vec![repo(
            "/work",
            vec![listed("a.rs", ChangeStatus::Modified, "f1")],
        )],
        cx,
    );
    arrive(
        &view,
        "a.rs",
        Some(&text(40, false)),
        Some(&text(40, true)),
        cx,
    );
    let hidden = |view: &Entity<ReviewView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |view, _| {
            view.shown_file(0)
                .gaps
                .iter()
                .map(|gap| gap.hidden)
                .collect::<Vec<_>>()
        })
    };
    // Three context lines around lines 10-12; the rest hidden, the tail's
    // length known.
    assert_eq!(hidden(&view, cx), [Some(7), Some(24)]);
    rows(&view, cx);
    click(cx, "review-expand-0-1-all", Modifiers::none());
    assert_eq!(hidden(&view, cx), [Some(7)]);
}

#[gpui_kit::test]
fn ticking_folds_and_a_changed_file_shows_it(cx: &mut TestAppContext) {
    let (view, cx) = view(cx);
    let files = |fingerprint: &str| {
        vec![repo(
            "/work",
            vec![
                listed("a.rs", ChangeStatus::Modified, fingerprint),
                listed("b.rs", ChangeStatus::Modified, "f1"),
            ],
        )]
    };
    list(&view, files("f1"), cx);
    arrive(
        &view,
        "a.rs",
        Some(&text(20, false)),
        Some(&text(20, true)),
        cx,
    );
    rows(&view, cx);

    click(cx, "review-viewed-0", Modifiers::none());
    view.read_with(cx, |view, _| {
        assert!(view.viewed("a.rs").viewed);
        assert!(view.list.folds.is_folded("a.rs"));
    });
    assert_eq!(
        rows(&view, cx)[..2],
        [CodeRow::File { file: 0 }, CodeRow::File { file: 1 }]
    );

    // Touched: the tick waits for the content, which is the same.
    list(&view, files("f2"), cx);
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.tick_of(view.review_file("a.rs").unwrap()),
            Tick::Unconfirmed
        );
        assert!(view.viewed("a.rs").viewed);
    });
    arrive(
        &view,
        "a.rs",
        Some(&text(20, false)),
        Some(&text(20, true)),
        cx,
    );
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.tick_of(view.review_file("a.rs").unwrap()),
            Tick::Viewed
        )
    });

    // Edited: changed since it was viewed.
    list(&view, files("f3"), cx);
    arrive(&view, "a.rs", Some(&text(20, false)), Some("other\n"), cx);
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.viewed("a.rs"),
            ViewedMark {
                viewed: false,
                stale: true
            }
        )
    });
}

#[gpui_kit::test]
fn a_shift_click_ticks_a_range(cx: &mut TestAppContext) {
    let (view, cx) = view(cx);
    let files = ["a.rs", "b.rs", "c.rs"]
        .map(|p| listed(p, ChangeStatus::Added, "f1"))
        .to_vec();
    list(&view, vec![repo("/work", files)], cx);
    rows(&view, cx);
    click(cx, "review-viewed-0", Modifiers::none());
    click(cx, "review-viewed-2", Modifiers::shift());
    view.read_with(cx, |view, _| {
        for path in ["a.rs", "b.rs", "c.rs"] {
            assert!(view.viewed(path).viewed, "{path}");
        }
    });
}

#[gpui_kit::test]
fn side_by_side_repositories_ask_which_one(cx: &mut TestAppContext) {
    let (view, cx) = view(cx);
    list(
        &view,
        vec![
            repo("/work/a", vec![listed("x.rs", ChangeStatus::Added, "f1")]),
            repo("/work/b", Vec::new()),
        ],
        cx,
    );
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-repo-a").is_some());
    assert!(cx.debug_bounds("review-repo-picker").is_none());
    click(cx, "review-repo-a", Modifiers::none());
    view.read_with(cx, |view, _| {
        assert_eq!(view.repo.as_deref(), Some(std::path::Path::new("/work/a")));
        assert_eq!(view.shown_count(), 1);
    });
    assert!(cx.debug_bounds("review-repo-picker").is_some());
}

#[gpui_kit::test]
fn a_comment_goes_to_the_agent_and_sits_below_its_line(cx: &mut TestAppContext) {
    let (view, cx) = view(cx);
    let inbox: Rc<RefCell<Vec<LineComment>>> = Rc::default();
    let sink = inbox.clone();
    cx.update(|_, cx| {
        let bus = cx.new(|_| CommentBus);
        cx.set_global(CommentBusGlobal(bus.clone()));
        cx.subscribe(&bus, move |_, change: &CommentChange, _| {
            if let CommentChange::Upsert(comment) = change {
                sink.borrow_mut().push(comment.clone());
            }
        })
        .detach();
    });
    list(
        &view,
        vec![repo(
            "/work",
            vec![listed("a.rs", ChangeStatus::Added, "f1")],
        )],
        cx,
    );
    arrive(&view, "a.rs", None, Some(&text(10, false)), cx);
    rows(&view, cx);
    view.update_in(cx, |view, window, cx| {
        view.list.selection.begin("a.rs".into(), 2, false);
        view.list.selection.extend(&"a.rs".into(), 4);
        view.list.selection.end();
        start_comment(view, window, cx);
    });
    cx.run_until_parked();
    assert!(rows(&view, cx).contains(&CodeRow::Draft { file: 0 }));
    assert!(cx.debug_bounds("review-draft").is_some());
    cx.simulate_input("why?");
    click(cx, "review-draft-agent", Modifiers::none());
    let comment = inbox.borrow().last().cloned().expect("handed to the agent");
    assert_eq!(comment.file, PathBuf::from("/work/a.rs"));
    assert_eq!((comment.start_line, comment.end_line), (3, 5));
    assert_eq!(comment.text, "why?");
    assert!(comment.excerpt.starts_with("+line 2"));

    // The draft's comment comes back as a card below its last line.
    view.update(cx, |view, cx| {
        view.set_comments(vec![LineComment { id: 7, ..comment }], cx)
    });
    let rows = rows(&view, cx);
    assert!(rows.contains(&CodeRow::Pending { file: 0, id: 7 }));
    assert!(cx.debug_bounds("review-comment-7").is_some());
}

#[gpui_kit::test]
fn the_header_stays_on_top_while_its_file_scrolls(cx: &mut TestAppContext) {
    let (view, cx) = view(cx);
    list(
        &view,
        vec![repo(
            "/work",
            vec![listed("a.rs", ChangeStatus::Added, "f1")],
        )],
        cx,
    );
    arrive(&view, "a.rs", None, Some(&text(200, false)), cx);
    rows(&view, cx);
    assert!(cx.debug_bounds("review-sticky-file-0").is_none());
    view.update(cx, |view, cx| {
        view.list.list_state.scroll_to(gpui_kit::ListOffset {
            item_ix: 2,
            offset_in_item: gpui_kit::px(0.),
        });
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-sticky-file-0").is_some());
}
