//! What the Code tab shows apart from the shared list rows: the loading
//! footer, the states shown instead of the list and the strip of threads
//! not on the diff.

use super::CodeTab;
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable};
use gpui_kit::{AnyElement, Context, SharedString, div, prelude::*, px, svg};

/// The off-diff strip's height at most; it scrolls beyond.
const OFF_DIFF_MAX_HEIGHT: f32 = 256.;

impl CodeTab {
    /// More pages follow: "Loading more files…" or a retry.
    pub(super) fn render_list_footer(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
                        .truncate()
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
}

impl CodeTab {
    /// "Load more files" below the file tree while more pages follow.
    pub(super) fn render_more_files(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.pages
            .last()
            .and_then(|page| page.next_cursor.as_ref())
            .map(|_| {
                let (label, retry) = match (&self.load_error, &self.loading) {
                    (Some(_), _) => ("Retry", true),
                    (None, Some(_)) => ("Loading more files...", false),
                    (None, None) => ("Load more files", false),
                };
                div()
                    .flex_none()
                    .p_2()
                    .child(
                        Button::new("pr-code-tree-more")
                            .outline()
                            .xsmall()
                            .w_full()
                            .label(label)
                            .disabled(self.loading.is_some() && !retry)
                            .on_click(cx.listener(move |this, _, _, cx| match retry {
                                true => this.retry(cx),
                                false => this.load_next(cx),
                            })),
                    )
                    .into_any_element()
            })
    }
}
