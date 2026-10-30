//! The Code tab in a window, fed pages and activity as the page would
//! read them (no session service behind it, so every write fails).

use super::super::conversation::fixtures::{commit, data, thread, window};
use super::super::fixtures::hours_ago;
use super::*;
use github::model::{DiffPage, DiffSide};
use gpui_kit::{Modifiers, TestAppContext, VisualTestContext};

fn tab(
    cx: &mut TestAppContext,
) -> (
    Entity<CodeTab>,
    Entity<PullRequestData>,
    &mut VisualTestContext,
) {
    let data = data(cx, None);
    let (tab, cx) = window(cx, {
        let data = data.clone();
        move |cx| CodeTab::new(data, cx)
    });
    (tab, data, cx)
}

/// Each path modified in one hunk at line 10: ctx, -old, +new, ctx.
fn patch(paths: &[&str]) -> String {
    paths
        .iter()
        .map(|p| {
            format!(
                "diff --git a/{p} b/{p}\n--- a/{p}\n+++ b/{p}\n\
                 @@ -10,3 +10,3 @@ fn f() {{\n ctx\n-old\n+new\n ctx\n"
            )
        })
        .collect()
}

/// A page of `paths` arrives as read with `cursor`.
fn arrive(
    tab: &Entity<CodeTab>,
    cx: &mut VisualTestContext,
    cursor: Option<&str>,
    paths: &[&str],
    next: Option<&str>,
) {
    let page = DiffPage {
        patch: patch(paths),
        truncated: false,
        next_cursor: next.map(str::to_owned),
        omitted_file_stats: Vec::new(),
    };
    tab.update(cx, |tab, cx| {
        let files = parse_page(&page);
        tab.page_arrived(
            Page {
                cursor: cursor.map(str::to_owned),
                fingerprint: fingerprint(&page),
                next_cursor: page.next_cursor.clone(),
                truncated: false,
                files,
            },
            cx,
        )
    });
    cx.run_until_parked();
}

fn set_activity(
    data: &Entity<PullRequestData>,
    cx: &mut VisualTestContext,
    threads: Vec<ReviewThread>,
    commits: Vec<github::model::Commit>,
) {
    data.update(cx, |data, cx| {
        data.activity = Some(Arc::new(Activity {
            author: None,
            reviewers: Vec::new(),
            comments: Vec::new(),
            comment_count: 0,
            comments_truncated: false,
            review_threads: threads,
            review_threads_truncated: false,
            commits,
            reactions: Vec::new(),
        }));
        cx.notify();
    });
    cx.run_until_parked();
}

fn rows(tab: &Entity<CodeTab>, cx: &mut VisualTestContext) -> Vec<CodeRow> {
    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.list_state.item_count(), tab.rows.len());
        tab.rows.clone()
    })
}

fn click(cx: &mut VisualTestContext, selector: &str, modifiers: Modifiers) {
    let bounds = cx
        .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .unwrap_or_else(|| panic!("nothing rendered as {selector}"));
    cx.simulate_click(bounds.center(), modifiers);
    cx.run_until_parked();
}

/// File header, gap ("9 unmodified lines · fn f() {"), chunk, tail.
fn file_rows(file: usize, stamp: u64) -> Vec<CodeRow> {
    vec![
        CodeRow::File { file },
        CodeRow::Gap {
            file,
            hunk: 0,
            stamp,
        },
        CodeRow::Chunk {
            file,
            chunk: 0,
            stamp,
        },
        CodeRow::Tail { file, stamp },
    ]
}

#[gpui_kit::test]
fn pages_extend_the_list_by_a_splice(cx: &mut TestAppContext) {
    let (tab, _, cx) = tab(cx);
    arrive(&tab, cx, None, &["src/b.rs", "src/a.rs"], Some("2"));
    let first = rows(&tab, cx);
    assert_eq!(
        first,
        [file_rows(0, 1), file_rows(1, 2), vec![CodeRow::Footer]].concat()
    );
    assert!(cx.debug_bounds("pr-code-footer").is_some());

    // A page for a cursor no page points to is dropped.
    arrive(&tab, cx, Some("9"), &["src/x.rs"], None);
    assert_eq!(rows(&tab, cx), first);

    arrive(&tab, cx, Some("2"), &["src/c.rs"], None);
    let second = rows(&tab, cx);
    assert_eq!(second[..8], first[..8], "the first page's rows stay");
    assert_eq!(second[8..], file_rows(2, 3)[..]);
    tab.read_with(cx, |tab, _| {
        let paths: Vec<&str> = tab
            .shown
            .iter()
            .map(|&s| tab.file(s).path.as_str())
            .collect();
        assert_eq!(paths, ["src/a.rs", "src/b.rs", "src/c.rs"]);
    });
    assert!(cx.debug_bounds("pr-code-footer").is_none());

    // The first page read again, changed: the pages after it go.
    arrive(&tab, cx, None, &["src/a.rs"], Some("2"));
    assert_eq!(
        rows(&tab, cx),
        [file_rows(0, 4), vec![CodeRow::Footer]].concat()
    );
}

#[gpui_kit::test]
fn ticking_folds_a_shift_click_ticks_a_range_and_a_failed_write_reverts(cx: &mut TestAppContext) {
    let (tab, data, cx) = tab(cx);
    arrive(&tab, cx, None, &["a.rs", "b.rs", "c.rs"], None);

    click(cx, "pr-code-viewed-0", Modifiers::none());
    assert_eq!(
        rows(&tab, cx)[..2],
        [CodeRow::File { file: 0 }, CodeRow::File { file: 1 }]
    );
    click(cx, "pr-code-viewed-2", Modifiers::shift());
    assert_eq!(
        rows(&tab, cx),
        (0..3)
            .map(|file| CodeRow::File { file })
            .collect::<Vec<_>>()
    );
    tab.read_with(cx, |tab, _| {
        assert!(
            ["a.rs", "b.rs", "c.rs"]
                .iter()
                .all(|p| tab.viewed.is_viewed(p))
        );
        assert!(tab.viewed.has_queued(), "batched, not sent yet");
    });

    // 400 ms after the last press the batch goes; without a service it
    // fails, the ticks come off and the page says so.
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    tab.read_with(cx, |tab, _| assert!(tab.viewed.has_queued()));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    tab.read_with(cx, |tab, _| {
        assert!(!tab.viewed.has_queued());
        assert!(!tab.viewed.is_viewed("a.rs"));
    });
    data.read_with(cx, |data, _| assert!(data.error.is_some()));
}

#[gpui_kit::test]
fn threads_sit_below_their_line_and_the_rest_off_the_diff(cx: &mut TestAppContext) {
    let (tab, data, cx) = tab(cx);
    arrive(&tab, cx, None, &["src/lib.rs"], None);
    let on_line = thread("t1", &[("c1", hours_ago(2))]);
    let mut outdated = thread("t2", &[("c2", hours_ago(2))]);
    outdated.line = None;
    let mut elsewhere = thread("t3", &[("c3", hours_ago(2))]);
    elsewhere.path = "src/other.rs".into();
    // Right 12 is the context row below `+new`; Left 11 the deleted row.
    let mut on_old = thread("t4", &[("c4", hours_ago(2))]);
    (on_old.side, on_old.line) = (DiffSide::Left, Some(11));
    set_activity(
        &data,
        cx,
        vec![on_line, outdated, elsewhere, on_old],
        Vec::new(),
    );

    let rows = rows(&tab, cx);
    let kinds: Vec<_> = rows
        .iter()
        .map(|row| match row {
            CodeRow::Chunk { chunk, .. } => format!("chunk {chunk}"),
            CodeRow::Thread { thread, .. } => format!("thread {thread}"),
            other => format!("{other:?}")
                .split_whitespace()
                .next()
                .unwrap()
                .to_owned(),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "File", "Gap", "chunk 0", "thread 3", "chunk 1", "thread 0", "Tail"
        ]
    );
    tab.read_with(cx, |tab, _| assert_eq!(tab.off_diff, [1, 2]));
    click(cx, "pr-code-off-diff", Modifiers::none());
    tab.read_with(cx, |tab, _| assert!(tab.off_diff_open));
}

#[gpui_kit::test]
fn a_selection_becomes_a_pending_review_comment(cx: &mut TestAppContext) {
    let (tab, _, cx) = tab(cx);
    arrive(&tab, cx, None, &["src/a.rs"], None);
    // Rows 1 and 2: `-old` (old line 11) and `+new` (new line 11).
    tab.update(cx, |tab, cx| {
        tab.selection.begin("src/a.rs".into(), 1, false);
        tab.selection.extend(&"src/a.rs".into(), 2);
        tab.selection.end();
        cx.notify();
    });
    // The first frame paints the rows and records the selection's end;
    // the next one floats the pill there.
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    click(cx, "pr-code-selection-comment", Modifiers::none());
    assert!(rows(&tab, cx).contains(&CodeRow::Draft { file: 0 }));

    cx.simulate_input("Rename this");
    // ⌘↵ adds it to the review.
    cx.simulate_keystrokes("secondary-enter");
    cx.run_until_parked();

    let pr = tab.read_with(cx, |tab, _| tab.pr.clone());
    let drafts = cx.update(|_, cx| review_drafts(cx));
    let pending = drafts.read_with(cx, |drafts, _| drafts.comments(&pr).to_vec());
    assert_eq!(pending.len(), 1);
    let comment = &pending[0];
    assert_eq!(comment.path, "src/a.rs");
    assert_eq!((comment.side, comment.line), (DiffSide::Right, 11));
    assert_eq!(
        (comment.start_side, comment.start_line),
        (Some(DiffSide::Left), Some(11))
    );
    assert_eq!(comment.body, "Rename this");
    assert_eq!(comment.excerpt, "-old\n+new");

    let rows = rows(&tab, cx);
    assert!(rows.contains(&CodeRow::Pending {
        file: 0,
        id: comment.id
    }));
    assert!(!rows.contains(&CodeRow::Draft { file: 0 }));
    assert!(
        cx.debug_bounds(Box::leak(
            format!("pr-code-pending-{}", comment.id).into_boxed_str()
        ))
        .is_some()
    );
    tab.read_with(cx, |tab, _| assert!(tab.selection.get().is_none()));
}

#[gpui_kit::test]
fn a_commit_scope_starts_over_without_comments(cx: &mut TestAppContext) {
    let (tab, data, cx) = tab(cx);
    arrive(&tab, cx, None, &["src/lib.rs"], None);
    set_activity(
        &data,
        cx,
        vec![thread("t1", &[("c1", hours_ago(2))])],
        vec![commit("abc1234def", hours_ago(3))],
    );
    tab.read_with(cx, |tab, _| assert!(tab.off_diff.is_empty()));

    tab.update(cx, |tab, cx| {
        tab.select_commit(Some("abc1234def".into()), cx)
    });
    cx.run_until_parked();
    assert!(rows(&tab, cx).is_empty());
    arrive(&tab, cx, None, &["src/lib.rs"], None);
    tab.update_in(cx, |tab, window, cx| {
        assert_eq!(tab.off_diff, [0], "a commit's lines are not the change's");
        tab.selection.begin("src/lib.rs".into(), 1, false);
        tab.selection.end();
        tab.start_comment(window, cx);
        assert!(tab.draft.is_none());
    });
}

#[gpui_kit::test]
fn hidden_tests_leave_the_list(cx: &mut TestAppContext) {
    let (tab, _, cx) = tab(cx);
    arrive(&tab, cx, None, &["src/a.rs", "tests/a.rs"], None);
    tab.update(cx, |tab, cx| tab.set_hide_tests(true, cx));
    cx.run_until_parked();
    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.test_files, 1);
        assert_eq!(tab.shown.len(), 1);
    });
    assert_eq!(rows(&tab, cx), file_rows(0, 1));
}

#[gpui_kit::test]
fn rows_show_plain_first_and_highlight_file_by_file(cx: &mut TestAppContext) {
    let (tab, _, cx) = tab(cx);
    let page = DiffPage {
        patch: patch(&["src/a.rs", "src/b.rs", "notes.unknown"]),
        truncated: false,
        next_cursor: None,
        omitted_file_stats: Vec::new(),
    };
    tab.update(cx, |tab, cx| {
        tab.page_arrived(
            Page {
                cursor: None,
                fingerprint: fingerprint(&page),
                next_cursor: None,
                truncated: false,
                files: parse_page(&page),
            },
            cx,
        );
        tab.sync_rows();
        assert!(tab.shown.iter().all(|&s| tab.file(s).diff.syntax.is_none()));
    });
    let plain = rows(&tab, cx);
    cx.run_until_parked();
    tab.read_with(cx, |tab, _| {
        let files: Vec<_> = tab.shown.iter().map(|&s| tab.file(s)).collect();
        assert!(files.iter().all(|f| f.highlighted));
        let with_syntax: Vec<&str> = files
            .iter()
            .filter(|f| f.diff.syntax.is_some())
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(with_syntax, ["src/a.rs", "src/b.rs"]);
    });
    assert_eq!(rows(&tab, cx), plain, "highlighting adds no rows");
}
