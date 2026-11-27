//! The list rows apart from the file headers and the comment rows: gaps,
//! diff chunks, the tail, notes; and the header that stays on top while
//! its file scrolls.

use super::context::{EXPAND_STEP, Reveal, ShownGap, gap_reveals};
use super::draft::{gutter_add, render_draft};
use super::header::render_file_header;
use super::list::{CodeHost, reveal};
use super::rows::{CodeRow, gap_label};
use crate::diff::render::{GutterNumbers, render_diff_chunk_rows, render_split_chunk_rows};
use crate::main_screen::right_panel::line_selection::{ChunkMarks, row_selection};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::{
    AnyElement, App, Context, FontWeight, SharedString, Window, div, prelude::*, px, rems,
};

/// The list item at `ix`.
pub fn render_row<V: CodeHost>(
    this: &mut V,
    ix: usize,
    window: &mut Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let Some(row) = this.list().rows.get(ix).cloned() else {
        return div().into_any_element();
    };
    let shown = |file: usize| (file < this.shown_count()).then_some(file);
    match row {
        CodeRow::File { file } => render_file_header(this, file, false, cx),
        CodeRow::Note { file, .. } => match shown(file) {
            Some(file) => render_note(this, file, cx),
            None => div().into_any_element(),
        },
        CodeRow::Gap { file, hunk, .. } => match shown(file) {
            Some(file) => render_gap(this, file, hunk, cx),
            None => div().into_any_element(),
        },
        CodeRow::Chunk { file, chunk, .. } => match shown(file) {
            Some(file) => render_chunk(this, file, chunk, window, cx),
            None => div().into_any_element(),
        },
        CodeRow::Tail { file, .. } => match shown(file) {
            Some(file) => {
                let after = this.shown_file(file).diff.hunks.len();
                render_gap(this, file, after, cx)
            }
            None => div().into_any_element(),
        },
        CodeRow::Thread { .. } | CodeRow::Pending { .. } => this.render_annotation(&row, cx),
        CodeRow::Draft { .. } => render_draft(this, cx),
        CodeRow::Footer => this.render_footer(cx),
    }
}

fn render_note<V: CodeHost>(this: &V, ix: usize, cx: &App) -> AnyElement {
    div()
        .px_4()
        .py_1p5()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(this.note_text(this.shown_file(ix)))
        .into_any_element()
}

/// The unchanged lines before shown hunk `before` (after the last when it
/// is the hunk count): their count and section on a hairline, and the
/// buttons revealing them where the file can be read.
fn render_gap<V: CodeHost>(this: &V, ix: usize, before: usize, cx: &mut Context<V>) -> AnyElement {
    let file = this.shown_file(ix);
    let Some(gap) = file.gaps.iter().find(|gap| gap.before == before).cloned() else {
        return div().into_any_element();
    };
    let label = gap_label(gap.hidden, gap.section.as_deref());
    let reveals = match file.has_context() {
        true => gap_reveals(&gap, file.patch_hunks.len()),
        false => Vec::new(),
    };
    let loading = this.text_loading(&file.path);
    let buttons: Vec<AnyElement> = reveals
        .into_iter()
        .map(|reveal| reveal_button(this, ix, &gap, reveal, loading, cx))
        .collect();
    div()
        .w_full()
        .h(px(24.))
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .bg(cx.theme().muted.opacity(0.35))
        .children(buttons)
        .child(hairline_label(label.into(), cx))
        .into_any_element()
}

fn reveal_button<V: CodeHost>(
    this: &V,
    ix: usize,
    gap: &ShownGap,
    which: Reveal,
    loading: bool,
    cx: &mut Context<V>,
) -> AnyElement {
    let (icon, tooltip, name) = match which {
        Reveal::Down => ("icons/chevron_down.svg", "Expand down", "down"),
        Reveal::Up => ("icons/chevron_up.svg", "Expand up", "up"),
        Reveal::All => ("icons/chevron_up_down.svg", "Expand all", "all"),
    };
    let gap_ix = gap.gap;
    let id = SharedString::from(format!(
        "{}-expand-{ix}-{gap_ix}-{name}",
        this.list().prefix
    ));
    let selector = id.to_string();
    let button = Button::new(id)
        .ghost()
        .xsmall()
        .loading(loading)
        .tooltip(tooltip)
        .on_click(cx.listener(move |this, _, window, cx| {
            // Shift shows the whole gap, as in T3 Code.
            let which = match window.modifiers().shift {
                true => Reveal::All,
                false => which,
            };
            reveal(this, ix, gap_ix, which, cx);
        }));
    let button = match which {
        // Between hunks a large gap offers "Expand all" as text.
        Reveal::All if gap.hidden.is_some_and(|h| h > EXPAND_STEP) => button.label("Expand all"),
        _ => button.icon(Icon::default().path(icon)),
    };
    div()
        .flex_none()
        .debug_selector(move || selector)
        .child(button)
        .into_any_element()
}

/// One chunk of a file's diff, its rows selectable.
fn render_chunk<V: CodeHost>(
    this: &V,
    ix: usize,
    chunk_ix: usize,
    window: &Window,
    cx: &Context<V>,
) -> AnyElement {
    let list = this.list();
    let file = this.shown_file(ix);
    let theme = cx.theme();
    let rem_size = window.rem_size();
    // The flat lines the chunk shows, for the marks and the anchor.
    let (lines, base_line) = match &file.split {
        Some(split) => {
            let Some(chunk) = split.chunks.get(chunk_ix) else {
                return div().into_any_element();
            };
            let flat = split.flat_starts.get(chunk.hunk).copied().unwrap_or(0);
            let rows = split.chunk_rows(chunk);
            let lines = rows.iter().flat_map(|row| [row.left, row.right]).flatten();
            let lo = lines.clone().min().unwrap_or(0) + flat;
            let hi = lines.max().unwrap_or(0) + flat;
            (lo..hi + 1, 0)
        }
        None => {
            let Some(chunk) = file.diff.chunked.chunks.get(chunk_ix) else {
                return div().into_any_element();
            };
            let start = chunk.flat_start;
            (start..start + chunk.lines.len(), start)
        }
    };
    let selection = this.can_comment().then(|| {
        let marks = ChunkMarks {
            highlight: list.selection.highlight(&file.path),
            color: theme.selection,
            marked: this.comment_marks(file, lines.clone(), cx),
            mark_color: theme.warning,
            anchor: list
                .anchors_selection_end(&file.path, lines.start, lines.len())
                .then(|| list.selection_anchor.clone()),
        };
        let mut selection = row_selection(
            file.path.clone(),
            base_line,
            marks,
            list.focus_handle.clone(),
            cx,
        );
        selection.gutter_add = Some(gutter_add(this, file.path.clone(), cx));
        selection
    });
    let scroll = (!list.code.wrap).then(|| file.scroll.clone());
    let rows = match &file.split {
        Some(split) => render_split_chunk_rows(
            &file.diff,
            split,
            &split.chunks[chunk_ix],
            &file.numbers,
            theme,
            rem_size,
            selection,
            scroll,
        ),
        None => render_diff_chunk_rows(
            &file.diff,
            &file.diff.chunked.chunks[chunk_ix],
            GutterNumbers::New,
            theme,
            rem_size,
            selection,
            scroll,
        ),
    };
    div()
        .w_full()
        .text_size(rems(0.78125))
        .line_height(rems(1.25).to_pixels(rem_size).round())
        .font_family(theme.mono_font_family.clone())
        .font_weight(FontWeight(400.0))
        .child(rows)
        .into_any_element()
}

/// The header of the file at the top of the view, kept there while its
/// rows scroll under it.
pub fn render_sticky_header<V: CodeHost>(this: &V, cx: &mut Context<V>) -> Option<AnyElement> {
    let list = this.list();
    let top = list.list_state.logical_scroll_top();
    let row = list.rows.get(top.item_ix)?;
    if matches!(row, CodeRow::File { .. }) && top.offset_in_item == px(0.) {
        return None;
    }
    let file = row.file()?;
    Some(
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .shadow_sm()
            .child(render_file_header(this, file, true, cx))
            .into_any_element(),
    )
}

/// Centred muted text on a hairline ("30 unmodified lines").
pub fn hairline_label(label: SharedString, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let line = || div().flex_1().h(px(1.)).bg(theme.border);
    div()
        .flex_1()
        .min_w_0()
        .px_2()
        .flex()
        .items_center()
        .gap_3()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(line())
        .child(div().min_w_0().truncate().child(label))
        .child(line())
        .into_any_element()
}
