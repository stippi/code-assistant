//! The Code tab's list rows apart from the file headers and the comment
//! rows: gaps, diff chunks, the tail, threads, the loading footer; the
//! states shown instead of the list, the strip of threads not on the diff
//! and the header that stays on top while its file scrolls.

use super::rows::{CodeRow, gap, gap_label};
use super::{CodeTab, Shown};
use crate::diff::render::{GutterNumbers, render_diff_chunk_rows};
use crate::main_screen::right_panel::line_selection::{ChunkMarks, row_selection};
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::{
    AnyElement, App, Context, FontWeight, SharedString, Window, div, prelude::*, px, rems, svg,
};

/// The off-diff strip's height at most; it scrolls beyond.
const OFF_DIFF_MAX_HEIGHT: f32 = 256.;

impl CodeTab {
    /// The list item at `ix`.
    pub(super) fn render_row(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        let shown = |file: usize| self.shown.get(file).copied();
        let shown_file = match &row {
            CodeRow::Note { file, .. }
            | CodeRow::Gap { file, .. }
            | CodeRow::Chunk { file, .. } => shown(*file),
            _ => None,
        };
        match row {
            CodeRow::File { file } => self.render_file_header(file, false, cx),
            CodeRow::Note { .. } => match shown_file {
                Some(shown) => self.render_note(shown, cx),
                None => div().into_any_element(),
            },
            CodeRow::Gap { hunk, .. } => match shown_file {
                Some(shown) => self.render_gap(shown, hunk, cx),
                None => div().into_any_element(),
            },
            CodeRow::Chunk { chunk, .. } => match shown_file {
                Some(shown) => self.render_chunk(shown, chunk, window, cx),
                None => div().into_any_element(),
            },
            CodeRow::Tail { .. } => {
                hairline_label("More unchanged context may be available".into(), false, cx)
            }
            CodeRow::Thread { thread, .. } => match self.thread_card(thread, cx) {
                Some(card) => annotation(card.into_any_element()),
                None => div().into_any_element(),
            },
            CodeRow::Pending { id, .. } => self.render_pending(id, cx),
            CodeRow::Draft { .. } => self.render_draft(cx),
            CodeRow::Footer => self.render_footer(cx),
        }
    }

    fn render_note(&self, shown: Shown, cx: &App) -> AnyElement {
        let file = self.file(shown);
        let text = if file.diff.is_binary {
            "Binary file not shown."
        } else if file.diff.too_large || file.additions + file.deletions > 0 {
            "GitHub did not send this file's changes (too large)."
        } else if file.orig_path.is_some() {
            "Renamed without changes."
        } else {
            "No content changes."
        };
        div()
            .px_4()
            .py_1p5()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text)
            .into_any_element()
    }

    fn render_gap(&self, shown: Shown, hunk: usize, cx: &App) -> AnyElement {
        let hunks = &self.file(shown).diff.hunks;
        let label = hunks
            .get(hunk)
            .and_then(|h| gap(hunk.checked_sub(1).and_then(|p| hunks.get(p)), h))
            .map(|(lines, section)| gap_label(lines, section))
            .unwrap_or_default();
        hairline_label(label.into(), true, cx)
    }

    /// One chunk of a file's diff, its rows selectable on the whole change.
    fn render_chunk(
        &self,
        shown: Shown,
        chunk_ix: usize,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let file = self.file(shown);
        let Some(chunk) = file.diff.chunked.chunks.get(chunk_ix) else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let rem_size = window.rem_size();
        let rows = chunk.lines.len();
        let selection = self.commit.is_none().then(|| {
            let marks = ChunkMarks {
                highlight: self.selection.highlight(&file.path, chunk.flat_start, rows),
                color: theme.selection,
                marked: self.pending_marks(file, chunk.flat_start, rows, cx),
                mark_color: theme.warning,
                anchor: self
                    .anchors_selection_end(&file.path, chunk.flat_start, rows)
                    .then(|| self.selection_anchor.clone()),
            };
            row_selection(
                file.path.clone(),
                chunk.flat_start,
                marks,
                self.focus_handle.clone(),
                cx,
            )
        });
        div()
            .w_full()
            .text_size(rems(0.78125))
            .line_height(rems(1.25).to_pixels(rem_size).round())
            .font_family(theme.mono_font_family.clone())
            .font_weight(FontWeight(400.0))
            .child(render_diff_chunk_rows(
                &file.diff,
                chunk,
                GutterNumbers::New,
                theme,
                rem_size,
                selection,
            ))
            .into_any_element()
    }

    fn render_footer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // In (or near) view: time for the next page.
        if self.loading.is_none() && self.load_error.is_none() {
            self.load_next(cx);
        }
        let row = div()
            .debug_selector(|| "pr-code-footer".into())
            .py_2()
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground);
        match &self.load_error {
            Some(_) => row
                .child("The rest of this diff could not be loaded.")
                .child(
                    Button::new("pr-code-retry")
                        .outline()
                        .xsmall()
                        .label("Retry")
                        .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                ),
            None => row.child("Loading more files…"),
        }
        .into_any_element()
    }

    /// What shows instead of the list: loading, a failure, no changes.
    pub(super) fn render_state(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let note = |text: SharedString| {
            div()
                .px_4()
                .py_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text)
        };
        if !self.pages.is_empty() {
            let more = self.pages.last().is_some_and(|p| p.next_cursor.is_some());
            if self.pages.iter().all(|p| p.files.is_empty()) && !more {
                return Some(
                    note(match self.commit {
                        None => "This pull request has no file changes.".into(),
                        Some(_) => "This commit has no file changes.".into(),
                    })
                    .into_any_element(),
                );
            }
            return None;
        }
        Some(match &self.load_error {
            Some(error) => note(error.clone())
                .flex()
                .flex_col()
                .items_start()
                .gap_2()
                .child(
                    Button::new("pr-code-retry")
                        .outline()
                        .xsmall()
                        .label("Retry")
                        .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                )
                .into_any_element(),
            None => note("Loading the diff…".into()).into_any_element(),
        })
    }

    /// "Conversations not on the current diff (N)": threads whose line no
    /// rendered row shows (outdated, on a hidden or not yet loaded file, or
    /// in a commit's scope), grouped by file.
    pub(super) fn render_off_diff(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.off_diff.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let more = self.pages.last().is_some_and(|p| p.next_cursor.is_some());
        let title = if more {
            "Conversations not on the diff loaded so far"
        } else {
            "Conversations not on the current diff"
        };
        let chevron = if self.off_diff_open {
            "icons/chevron_down.svg"
        } else {
            "icons/chevron_right.svg"
        };
        let header = div()
            .id("pr-code-off-diff")
            .debug_selector(|| "pr-code-off-diff".into())
            .px_4()
            .py_2()
            .flex()
            .items_center()
            .gap_1p5()
            .text_xs()
            .text_color(muted)
            .cursor_pointer()
            .child(title)
            .child(svg().size(px(12.)).path(chevron).text_color(muted))
            .child(self.off_diff.len().to_string())
            .on_click(cx.listener(|this, _, _, cx| {
                this.off_diff_open = !this.off_diff_open;
                cx.notify();
            }));
        let border = theme.border;
        let mut strip = div()
            .flex_none()
            .border_b_1()
            .border_color(border)
            .child(header);
        if self.off_diff_open {
            let activity = self.activity.clone();
            let threads = activity.as_ref().map_or(&[][..], |a| &a.review_threads[..]);
            // Grouped by file, files in the order their first thread came.
            let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
            for &ix in &self.off_diff {
                let path = threads[ix].path.as_str();
                match groups.iter_mut().find(|(p, _)| *p == path) {
                    Some((_, ixs)) => ixs.push(ix),
                    None => groups.push((path, vec![ix])),
                }
            }
            let mut body = div().px_4().pb_3().flex().flex_col().gap_3();
            for (path, ixs) in groups {
                let mut group = div().flex().flex_col().gap_1().child(
                    div()
                        .px_3()
                        .text_xs()
                        .text_color(muted)
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(path.to_owned()),
                );
                for ix in ixs {
                    let line = threads[ix].line;
                    if let Some(card) = self.thread_card(ix, cx) {
                        group = group
                            .children(line.map(|line| {
                                div()
                                    .px_3()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("Line {line}"))
                            }))
                            .child(card);
                    }
                }
                body = body.child(group);
            }
            strip = strip.child(
                div()
                    .id("pr-code-off-diff-threads")
                    .max_h(px(OFF_DIFF_MAX_HEIGHT))
                    .overflow_y_scroll()
                    .child(body),
            );
        }
        Some(strip.into_any_element())
    }

    /// The header of the file at the top of the view, kept there while its
    /// rows scroll under it.
    pub(super) fn render_sticky_header(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let top = self.list_state.logical_scroll_top();
        let file = match self.rows.get(top.item_ix)? {
            CodeRow::File { .. } if top.offset_in_item == px(0.) => return None,
            CodeRow::File { file }
            | CodeRow::Note { file, .. }
            | CodeRow::Gap { file, .. }
            | CodeRow::Chunk { file, .. }
            | CodeRow::Tail { file, .. }
            | CodeRow::Thread { file, .. }
            | CodeRow::Pending { file, .. }
            | CodeRow::Draft { file } => *file,
            CodeRow::Footer => return None,
        };
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .shadow_sm()
                .child(self.render_file_header(file, true, cx))
                .into_any_element(),
        )
    }
}

/// A row below a diff line: a thread or comment card, indented a little.
pub(super) fn annotation(card: AnyElement) -> AnyElement {
    div().px_3().py_1().child(card).into_any_element()
}

/// Centred muted text on a hairline ("30 unmodified lines").
fn hairline_label(label: SharedString, above: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let line = || div().flex_1().h(px(1.)).bg(theme.border);
    div()
        .w_full()
        .px_3()
        .when(above, |d| d.pt_2().pb_1())
        .when(!above, |d| d.pt_1().pb_2())
        .flex()
        .items_center()
        .gap_3()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(line())
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(label),
        )
        .child(line())
        .into_any_element()
}
