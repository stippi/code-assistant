//! New comments on the diff: the "+" over a line number, or rows selected
//! with the shared line selection, which show the pill (copy, comment);
//! the "+" and "comment" open a box below the last row, from which the
//! comment goes to the agent's draft ("Add to agent") or, where the host
//! keeps comments of its own, there (a pull request's pending review).

use super::anchors::{RangeRef, range_of};
use super::file::LoadedFile;
use super::list::{CodeHost, CodeList};
use crate::comments::{CommentChange, floating, selection_pill};
use crate::diff::model::DiffLine;
use crate::diff::rows::GutterAdd;
use code_assistant_core::line_comments::LineComment;
use github::model::DiffSide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, InputEvent, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::{
    AnyElement, ClipboardItem, Context, Entity, SharedString, Subscription, Window, div, point,
    prelude::*, px,
};
use similar::ChangeTag;
use std::path::PathBuf;
use std::rc::Rc;

/// The box a new comment is written in, below the last selected row.
pub struct Draft {
    pub path: String,
    /// The commented rows, flat line indices, inclusive.
    pub lo: usize,
    pub hi: usize,
    pub at: RangeRef,
    /// The agent's comment it edits (0: a new one).
    pub editing: u64,
    input: Entity<TextareaState>,
    _subscription: Subscription,
}

/// A file's rows in flat order (as the selection counts them).
fn flat_lines(file: &LoadedFile) -> impl Iterator<Item = &DiffLine> {
    file.diff.hunks.iter().flat_map(|hunk| &hunk.lines)
}

/// Rows `lo..=hi` as shown, `+`/`-`/` ` prefixed.
pub fn excerpt(file: &LoadedFile, lo: usize, hi: usize) -> String {
    flat_lines(file)
        .skip(lo)
        .take(hi + 1 - lo)
        .map(|line| {
            let prefix = match line.tag {
                ChangeTag::Insert => '+',
                ChangeTag::Delete => '-',
                ChangeTag::Equal => ' ',
            };
            format!("{prefix}{}", line.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// "12" or "10-12", the lines of `at`, `L` for the old file's.
pub fn lines_label(at: &RangeRef) -> String {
    let side = |side: DiffSide| match side {
        DiffSide::Left => "L",
        DiffSide::Right => "",
    };
    match at.start {
        Some(start) if start != at.end => format!(
            "{}{}-{}{}",
            side(start.side),
            start.line,
            side(at.end.side),
            at.end.line
        ),
        _ => format!("{}{}", side(at.end.side), at.end.line),
    }
}

/// The agent's line comment on rows `lo..=hi` of `file`: their new-file
/// lines, or the old file's when only deleted rows are selected.
pub fn agent_comment(
    file: PathBuf,
    numbers: &[(ChangeTag, usize, usize)],
    lo: usize,
    hi: usize,
    excerpt: String,
    text: String,
) -> Option<LineComment> {
    let rows = numbers.get(lo..=hi)?;
    let new_side: Vec<usize> = rows
        .iter()
        .filter(|(tag, ..)| *tag != ChangeTag::Delete)
        .map(|&(_, _, new)| new)
        .collect();
    let (old_side, lines) = if new_side.is_empty() {
        (true, rows.iter().map(|&(_, old, _)| old).collect())
    } else {
        (false, new_side)
    };
    Some(LineComment {
        id: 0,
        file,
        start_line: *lines.iter().min()?,
        end_line: *lines.iter().max()?,
        old_side,
        in_diff: true,
        on_message: false,
        on_file: false,
        excerpt,
        text,
    })
}

/// The selected file (by its shown index) and rows, inclusive.
pub fn selected_file<V: CodeHost>(this: &V) -> Option<(usize, usize, usize)> {
    let selected = this.list().selection.get()?;
    let ix = this.shown_ix(&selected.key)?;
    let file = this.shown_file(ix);
    let (lo, hi) = selected.range();
    let hi = hi.min(file.numbers.len().checked_sub(1)?);
    (lo <= hi).then_some((ix, lo, hi))
}

/// The selected rows' text, whole lines.
pub fn selected_text<V: CodeHost>(this: &V) -> Option<String> {
    let (ix, lo, hi) = selected_file(this)?;
    Some(
        flat_lines(this.shown_file(ix))
            .skip(lo)
            .take(hi + 1 - lo)
            .map(|line| line.text.as_ref())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

pub fn copy_selection<V: CodeHost>(this: &mut V, cx: &mut Context<V>) {
    if let Some(text) = selected_text(this) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
}

/// The "+" over the line numbers of `path`: a press selects like any
/// other, releasing it opens the comment box on what is selected then.
pub fn gutter_add<V: CodeHost>(this: &V, path: String, cx: &Context<V>) -> GutterAdd {
    let theme = cx.theme();
    let list = this.list();
    let focus = list.focus_handle.clone();
    let view = cx.entity().downgrade();
    let (press, release, repaint) = (view.clone(), view.clone(), view);
    GutterAdd {
        show: list.draft.is_none() && !list.selection.is_dragging(),
        color: theme.info,
        glyph: gpui_kit::white(),
        on_press: Rc::new(move |line, window, cx| {
            window.focus(&focus, cx);
            press
                .update(cx, |view, cx| {
                    let list = view.list_mut();
                    list.selection.begin(path.clone(), line, false);
                    list.comment_on_release = true;
                    cx.notify();
                })
                .ok();
        }),
        on_release: Rc::new(move |window, cx| {
            release
                .update(cx, |view, cx| {
                    if std::mem::take(&mut view.list_mut().comment_on_release) {
                        start_comment(view, window, cx);
                    }
                })
                .ok();
        }),
        repaint: Rc::new(move |_, cx| {
            repaint.update(cx, |_, cx| cx.notify()).ok();
        }),
    }
}

/// Open the comment box below the selection.
pub fn start_comment<V: CodeHost>(this: &mut V, window: &mut Window, cx: &mut Context<V>) {
    open_comment(this, None, window, cx);
}

/// Open the comment box below the selection, holding `editing`'s text
/// when it edits one of the agent's comments.
pub fn open_comment<V: CodeHost>(
    this: &mut V,
    editing: Option<&LineComment>,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    if !this.can_comment() {
        return;
    }
    let Some((ix, lo, hi)) = selected_file(this) else {
        return;
    };
    let file = this.shown_file(ix);
    let Some(at) = range_of(&file.numbers, lo, hi) else {
        return;
    };
    let path = file.path.clone();
    let text = editing.map(|comment| comment.text.clone());
    let input = cx.new(|cx| {
        let mut input = TextareaState::new(window, cx)
            .auto_grow(2, 8)
            .placeholder("Add a comment…");
        if let Some(text) = text {
            input.set_value(text, window, cx);
        }
        input
    });
    input.update(cx, |input, cx| input.focus(window, cx));
    let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
        if let InputEvent::PressEnter {
            secondary: true, ..
        } = event
        {
            match this.comment_host_target() {
                Some(_) => this.add_comment_to_host(cx),
                None => add_to_agent(this, cx),
            }
        }
    });
    this.list_mut().draft = Some(Draft {
        path,
        lo,
        hi,
        at,
        editing: editing.map_or(0, |comment| comment.id),
        input,
        _subscription: subscription,
    });
    this.place_annotations(cx);
}

/// The comment box's text, trimmed; `None` while empty.
pub fn draft_text<V: CodeHost>(this: &V, cx: &Context<V>) -> Option<String> {
    let draft = this.list().draft.as_ref()?;
    let text = draft.input.read(cx).value().trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// The comment box's file (by shown index) and excerpt.
pub fn draft_file<V: CodeHost>(this: &V) -> Option<(usize, String)> {
    let draft = this.list().draft.as_ref()?;
    let ix = this.shown_ix(&draft.path)?;
    Some((ix, excerpt(this.shown_file(ix), draft.lo, draft.hi)))
}

/// The comment box's comment into the agent's draft (the composer).
pub fn add_to_agent<V: CodeHost>(this: &mut V, cx: &mut Context<V>) {
    let Some(text) = draft_text(this, cx) else {
        return;
    };
    let comment = this.list().draft.as_ref().and_then(|draft| {
        let (ix, excerpt) = draft_file(this)?;
        agent_comment(
            this.comment_file(&draft.path),
            &this.shown_file(ix).numbers,
            draft.lo,
            draft.hi,
            excerpt,
            text,
        )
        .map(|comment| LineComment {
            id: draft.editing,
            ..comment
        })
    });
    if let Some(comment) = comment {
        crate::comments::report(CommentChange::Upsert(comment), cx);
    }
    close_draft(this, cx);
}

pub fn close_draft<V: CodeHost>(this: &mut V, cx: &mut Context<V>) {
    let list = this.list_mut();
    list.draft = None;
    list.selection.clear();
    this.place_annotations(cx);
}

/// The pill under the selection's last row (not while the comment box is
/// open).
pub fn render_floating<V: CodeHost>(
    this: &mut V,
    window: &mut Window,
    cx: &mut Context<V>,
) -> Option<AnyElement> {
    let list = this.list_mut();
    // Written by the last paint; taken so a row scrolled away hides it.
    let row = list.selection_anchor.take();
    let dismissed = list.selection.get() == list.pill_dismissed.as_ref();
    let position = row
        .filter(|_| {
            list.draft.is_none()
                && list.selection.get().is_some()
                && !list.selection.is_dragging()
                && !dismissed
        })
        .map(|b| point(b.right() - px(72.), b.bottom() + px(2.)));
    list.anchor.track(position, window);
    let view = cx.entity().downgrade();
    let view_for_comment = view.clone();
    let dismiss = cx.listener(|this: &mut V, _: &gpui_kit::MouseDownEvent, _, cx| {
        let list = this.list_mut();
        list.pill_dismissed = list.selection.get().cloned();
        cx.notify();
    });
    let id = SharedString::from(format!("{}-selection", this.list().prefix));
    Some(floating(
        position?,
        div().on_mouse_down_out(dismiss).child(selection_pill(
            id,
            move |_, cx| {
                view.update(cx, |view, cx| copy_selection(view, cx)).ok();
            },
            move |window, cx| {
                view_for_comment
                    .update(cx, |view, cx| start_comment(view, window, cx))
                    .ok();
            },
            cx,
        )),
    ))
}

/// A row below a diff line: a thread or comment card, indented a little.
pub fn annotation(card: AnyElement) -> AnyElement {
    div().px_3().py_1().child(card).into_any_element()
}

/// The comment box row.
pub fn render_draft<V: CodeHost>(this: &V, cx: &mut Context<V>) -> AnyElement {
    let list: &CodeList = this.list();
    let Some(draft) = &list.draft else {
        return div().into_any_element();
    };
    let prefix = list.prefix;
    let theme = cx.theme();
    let host_target = this.comment_host_target();
    let agent_id = format!("{prefix}-draft-agent");
    let agent = Button::new(SharedString::from(agent_id.clone()))
        .label("Add to agent")
        .xsmall()
        .on_click(cx.listener(|this, _, _, cx| add_to_agent(this, cx)));
    let agent = match host_target {
        Some(_) => agent.outline(),
        None => agent.primary().tooltip("⌘↵"),
    };
    let card = div()
        .id(SharedString::from(format!("{prefix}-draft")))
        .debug_selector(move || format!("{prefix}-draft"))
        .flex()
        .flex_col()
        .gap_1p5()
        .p_2()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .capture_action(cx.listener(|this, _: &Escape, _, cx| {
            close_draft(this, cx);
            cx.stop_propagation();
        }))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "Comment on {}:{}",
                    draft.path,
                    lines_label(&draft.at)
                )),
        )
        .child(Textarea::new(&draft.input))
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(div().flex_1())
                .child(
                    Button::new(SharedString::from(format!("{prefix}-draft-cancel")))
                        .label("Cancel")
                        .xsmall()
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| close_draft(this, cx))),
                )
                .child(div().debug_selector(move || agent_id).child(agent))
                .children(host_target.map(|label| {
                    Button::new(SharedString::from(format!("{prefix}-draft-host")))
                        .label(label)
                        .xsmall()
                        .primary()
                        .tooltip("⌘↵")
                        .on_click(cx.listener(|this, _, _, cx| this.add_comment_to_host(cx)))
                })),
        );
    annotation(card.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_screen::right_panel::code_view::anchors::LineRef;
    use ChangeTag::*;

    #[test]
    fn agent_comments_take_new_lines_or_else_old_ones() {
        let numbers = [
            (Equal, 10, 10),
            (Delete, 11, 11),
            (Insert, 12, 11),
            (Equal, 12, 12),
        ];
        let file = || PathBuf::from("src/a.rs");
        let comment = agent_comment(file(), &numbers, 0, 2, "x".into(), "why".into()).unwrap();
        assert_eq!(
            (comment.start_line, comment.end_line, comment.old_side),
            (10, 11, false)
        );
        assert_eq!(comment.file, PathBuf::from("src/a.rs"));
        assert!(comment.in_diff);
        let deleted = agent_comment(file(), &numbers, 1, 1, "x".into(), "why".into()).unwrap();
        assert_eq!(
            (deleted.start_line, deleted.end_line, deleted.old_side),
            (11, 11, true)
        );
        assert!(agent_comment(file(), &numbers, 2, 7, "x".into(), "y".into()).is_none());
    }

    #[test]
    fn line_labels_name_the_old_side() {
        let right = |line| LineRef {
            side: DiffSide::Right,
            line,
        };
        let left = |line| LineRef {
            side: DiffSide::Left,
            line,
        };
        assert_eq!(
            lines_label(&RangeRef {
                start: None,
                end: right(12)
            }),
            "12"
        );
        assert_eq!(
            lines_label(&RangeRef {
                start: Some(left(11)),
                end: right(13)
            }),
            "L11-13"
        );
    }
}
