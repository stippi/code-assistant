//! Diff card renderer for `edit`, `replace_in_file`, `write_file`, and
//! `delete_files` tool blocks.
//!
//! Renders file-editing tools as bordered cards with:
//! - Header: file icon + path, red ✕ on error, chevron toggle
//! - Body: unified diff view (edit, replace_in_file), content preview
//!   (write_file), or deleted paths list (delete_files)
//!
//! Replaces the old parameter-renderer-based rendering for these tools.

use super::{CardRenderContext, ToolBlockRenderer, ToolBlockStyle, animated_card_body};
use crate::blocks::{BlockView, ToolUseBlock};
use crate::shared::file_icons;
use code_assistant_core::ui::ToolStatus;
use gpui::prelude::FluentBuilder;
use gpui::{
    ClickEvent, Context, Element, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px, rems,
};
use similar::{ChangeTag, TextDiff};

// ---------------------------------------------------------------------------
// DiffCardRenderer
// ---------------------------------------------------------------------------

pub struct DiffCardRenderer;

impl ToolBlockRenderer for DiffCardRenderer {
    fn supported_tools(&self) -> Vec<String> {
        vec![
            "edit".to_string(),
            "replace_in_file".to_string(),
            "write_file".to_string(),
            "delete_files".to_string(),
        ]
    }

    fn style(&self) -> ToolBlockStyle {
        ToolBlockStyle::Card
    }

    fn describe(&self, tool: &ToolUseBlock) -> String {
        let path = extract_path_or_paths(tool);
        if path.is_empty() {
            tool.name.replace('_', " ")
        } else {
            path
        }
    }

    fn render(
        &self,
        tool: &ToolUseBlock,
        is_generating: bool,
        theme: &gpui_component::theme::Theme,
        card_ctx: Option<&CardRenderContext>,
        window: &mut Window,
        cx: &mut Context<BlockView>,
    ) -> Option<gpui::AnyElement> {
        let card_ctx = card_ctx?;
        let rem_size = window.rem_size();

        // We need at least one parameter to show anything.
        if tool.parameters.is_empty() {
            return None;
        }

        let path_label = extract_path_or_paths(tool);
        let has_error = tool.status == ToolStatus::Error;
        let is_dark = theme.background.l < 0.5;

        let scale = card_ctx.animation_scale;
        let is_collapsed = card_ctx.is_collapsed;

        let header_bg = if is_dark {
            gpui::hsla(0.0, 0.0, 0.15, 1.0)
        } else {
            gpui::hsla(0.0, 0.0, 0.93, 1.0)
        };

        // --- Card container ---

        let mut card = div()
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .overflow_hidden();

        // --- Header ---
        let header_text_color = theme.muted_foreground;

        let icon = file_icons::get().get_tool_icon(&tool.name);
        let icon_fallback = match tool.name.as_str() {
            "edit" => "✎",
            "replace_in_file" => "⇄",
            "write_file" => "✎",
            "delete_files" => "🗑",
            _ => "📄",
        };

        let chevron_icon = if is_collapsed {
            file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN)
        } else {
            file_icons::get().get_type_icon(file_icons::CHEVRON_UP)
        };

        let mut header_left = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .min_w_0()
            .flex_grow(1.0)
            .child(file_icons::render_icon_container(
                &icon,
                13.0,
                header_text_color,
                icon_fallback,
            ));

        let header_label = if !path_label.is_empty() {
            abbreviate_path(&path_label)
        } else {
            tool.name.replace('_', " ")
        };
        header_left = header_left.child(
            div()
                .text_size(rems(0.75))
                .text_color(header_text_color)
                .overflow_hidden()
                .child(header_label),
        );

        let mut header_right = div().flex().flex_row().items_center().gap_1();
        if has_error {
            header_right = header_right.child(
                gpui::svg()
                    .size(px(13.0))
                    .path(SharedString::from("icons/close.svg"))
                    .text_color(theme.danger),
            );
        }
        // Diff/File toggle button for write_file with original_content
        if tool.name == "write_file" && write_file_has_original_content(tool) {
            let diff_mode = card_ctx.write_file_diff_mode;
            let label: SharedString = if diff_mode { "diff" } else { "file" }.into();
            let btn_text_color = if diff_mode {
                theme.accent
            } else {
                header_text_color
            };
            header_right = header_right.child(
                div()
                    .id(SharedString::from(format!("diff-toggle-{}", tool.id)))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_1p5()
                    .py(px(2.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(header_text_color.opacity(0.1)))
                    .text_size(rems(0.6875))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(btn_text_color)
                    .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                        view.toggle_write_file_diff_mode(cx);
                    }))
                    .child(label),
            );
        }
        // Chevron — highlights on header hover via group
        header_right = header_right.child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded(px(6.))
                .group_hover("diff-header", |s| s.bg(header_text_color.opacity(0.1)))
                .child(file_icons::render_icon(
                    &chevron_icon,
                    14.0,
                    header_text_color.opacity(0.4),
                    "▾",
                )),
        );

        // Header corners: all rounded when collapsed, only top when expanded.
        let header = div()
            .id(SharedString::from(format!("diff-header-{}", tool.id)))
            .group("diff-header")
            .px_3()
            .py_1p5()
            .bg(header_bg)
            .cursor_pointer()
            .flex()
            .flex_row()
            .justify_between()
            .items_center()
            .map(|d| {
                if scale <= 0.0 {
                    d.rounded(px(4.))
                } else {
                    d.rounded_t(px(4.))
                }
            })
            .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                view.toggle_tool_collapsed(cx);
            }))
            .child(header_left)
            .child(header_right);

        card = card.child(header);

        // --- Body (animated) ---
        if scale > 0.0 {
            let body_bg = if is_dark {
                gpui::hsla(0.0, 0.0, 0.08, 1.0)
            } else {
                gpui::hsla(0.0, 0.0, 0.97, 1.0)
            };

            let body_content = match tool.name.as_str() {
                "edit" => render_edit_body(tool, is_generating, theme, rem_size),
                "replace_in_file" => render_replace_body(tool, is_generating, theme, rem_size),
                "write_file" => {
                    render_write_body(tool, theme, rem_size, card_ctx.write_file_diff_mode)
                }
                "delete_files" => render_delete_body(tool, theme),
                _ => None,
            };

            let error_element = if has_error {
                tool.output
                    .as_deref()
                    .filter(|o| !o.is_empty())
                    .map(|output| {
                        div()
                            .px_3()
                            .py_1p5()
                            .text_size(rems(0.78125))
                            .text_color(theme.danger)
                            .child(output.to_string())
                            .into_any_element()
                    })
            } else {
                None
            };

            if body_content.is_some() || error_element.is_some() {
                // Round line height to whole pixels to avoid sub-pixel gaps
                // between adjacent rows with different background colors.
                let line_height_px = rems(1.25).to_pixels(rem_size).round();

                let mut body_inner = div()
                    .w_full()
                    .py_1()
                    .bg(body_bg)
                    .rounded_b(px(4.))
                    .flex()
                    .flex_col()
                    .text_size(rems(0.78125))
                    .line_height(line_height_px)
                    .font_family("Menlo")
                    .font_weight(FontWeight(400.0))
                    .overflow_hidden();

                if let Some(content) = body_content {
                    body_inner = body_inner.child(content);
                }
                if let Some(error) = error_element {
                    body_inner = body_inner.child(error);
                }

                card = card.child(animated_card_body(
                    body_inner,
                    scale,
                    card_ctx.content_height.clone(),
                ));
            }
        }

        Some(card.into_any_element())
    }
}

// ---------------------------------------------------------------------------
// Per-tool body rendering
// ---------------------------------------------------------------------------

/// Render body for the `edit` tool.
///
/// During streaming (`is_generating`), parameters are still being built up so
/// we show raw red/green blocks.  Once the tool is complete we compute a real
/// unified diff so only the actually-changed lines are highlighted — matching
/// what is shown after a session reload.
fn render_edit_body(
    tool: &ToolUseBlock,
    is_generating: bool,
    theme: &gpui_component::theme::Theme,
    rem_size: gpui::Pixels,
) -> Option<gpui::AnyElement> {
    let old_text = get_param(tool, "old_text");
    let new_text = get_param(tool, "new_text");

    if is_generating {
        // Streaming: show whatever we have so far as raw blocks
        let mut children: Vec<gpui::AnyElement> = Vec::new();
        if let Some(old) = old_text.filter(|s| !s.is_empty()) {
            children.push(render_streaming_block(old, true, theme));
        }
        if let Some(new) = new_text.filter(|s| !s.is_empty()) {
            children.push(render_streaming_block(new, false, theme));
        }
        if children.is_empty() {
            return None;
        }
        Some(div().flex().flex_col().children(children).into_any())
    } else {
        // Completed: compute a proper unified diff
        let start_lines = parse_match_start_lines(tool);
        let start_line = start_lines.first().copied();
        match (old_text, new_text) {
            (Some(old), Some(new)) if !old.is_empty() || !new.is_empty() => {
                Some(render_unified_diff(old, new, theme, start_line, rem_size))
            }
            (Some(old), None) if !old.is_empty() => Some(render_streaming_block(old, true, theme)),
            (None, Some(new)) if !new.is_empty() => Some(render_streaming_block(new, false, theme)),
            _ => None,
        }
    }
}

/// Render body for the `replace_in_file` tool.
///
/// Same streaming/completed split as `render_edit_body`: during streaming we
/// show raw search/replace blocks, after completion we show unified diffs.
fn render_replace_body(
    tool: &ToolUseBlock,
    is_generating: bool,
    theme: &gpui_component::theme::Theme,
    rem_size: gpui::Pixels,
) -> Option<gpui::AnyElement> {
    let diff_text = get_param(tool, "diff")?;
    if diff_text.is_empty() {
        return None;
    }

    let sections = parse_diff_sections(diff_text);
    if sections.is_empty() {
        return None;
    }

    let start_lines = parse_match_start_lines(tool);

    let children: Vec<gpui::AnyElement> = sections
        .into_iter()
        .enumerate()
        .map(|(i, section)| {
            if is_generating || section.in_search || section.in_replace {
                // Streaming or incomplete section: show raw blocks
                render_streaming_diff_section(&section, theme)
            } else {
                // Completed section: compute proper unified diff
                let start_line = start_lines.get(i).copied();
                render_unified_diff(
                    &section.search_content,
                    &section.replace_content,
                    theme,
                    start_line,
                    rem_size,
                )
            }
        })
        .collect();

    Some(
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(children)
            .into_any(),
    )
}

/// Render body for the `write_file` tool.
///
/// When `diff_mode` is true and the tool output contains `original_content`
/// (indicating an existing file was overwritten), renders a unified diff.
/// Otherwise falls back to all-green additions with line numbers.
fn render_write_body(
    tool: &ToolUseBlock,
    theme: &gpui_component::theme::Theme,
    rem_size: gpui::Pixels,
    diff_mode: bool,
) -> Option<gpui::AnyElement> {
    let content = get_param(tool, "content")?;
    if content.is_empty() {
        return None;
    }

    // Try to extract original_content from the tool output JSON
    let original_content = tool
        .output
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| {
            v.get("original_content")
                .and_then(|c| c.as_str())
                .map(String::from)
        });

    // If we have original content and diff mode is on, show a unified diff
    if diff_mode && let Some(ref original) = original_content {
        return Some(render_unified_diff(
            original,
            content,
            theme,
            Some(1),
            rem_size,
        ));
    }

    // Fall back to all-green additions (new file or diff mode toggled off)
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let gutter_width = total_lines.to_string().len();

    // Gutter width in pixels (~0.5rem per digit + 0.75rem padding)
    let gutter_px = rems(gutter_width as f32 * 0.5 + 0.75)
        .to_pixels(rem_size)
        .round();

    let (row_bg, text_color) = added_row_colors(theme);
    let gutter_color = text_color.opacity(0.5);

    Some(
        div()
            .flex()
            .flex_col()
            .children(lines.into_iter().enumerate().map(|(i, line)| {
                let line_num = i + 1;
                let gutter_text = format!("{:>width$}", line_num, width = gutter_width);

                let mut row = div().w_full().flex().flex_row().items_start();
                if let Some(bg) = row_bg {
                    row = row.bg(bg);
                }

                // Gutter
                row = row.child(
                    div()
                        .flex_none()
                        .w(gutter_px)
                        .pl_1p5()
                        .pr_1()
                        .text_color(gutter_color)
                        .child(gutter_text),
                );

                // Content
                row = row.child(
                    div()
                        .flex_grow(1.0)
                        .overflow_x_hidden()
                        .pl_1()
                        .pr_3()
                        .text_color(text_color)
                        .child(line.to_string()),
                );

                row.into_any()
            }))
            .into_any(),
    )
}

/// Render body for the `delete_files` tool — all-red deletions showing paths.
fn render_delete_body(
    tool: &ToolUseBlock,
    theme: &gpui_component::theme::Theme,
) -> Option<gpui::AnyElement> {
    let paths_raw = get_param(tool, "paths")?;
    if paths_raw.is_empty() {
        return None;
    }

    // The paths parameter is a JSON array of strings.
    let paths: Vec<String> =
        serde_json::from_str(paths_raw).unwrap_or_else(|_| vec![paths_raw.to_string()]);

    if paths.is_empty() {
        return None;
    }

    let (row_bg, text_color) = deleted_row_colors(theme);

    Some(
        div()
            .flex()
            .flex_col()
            .children(paths.into_iter().map(|path| {
                let mut row = div().w_full().px_3().text_color(text_color).child(path);
                if let Some(bg) = row_bg {
                    row = row.bg(bg);
                }
                row.into_any()
            }))
            .into_any(),
    )
}

// ---------------------------------------------------------------------------
// Diff rendering
// ---------------------------------------------------------------------------

/// Normalize text for diff display.
///
/// LLMs frequently emit a spurious leading `\n` at the start of `old_text` or
/// `new_text` JSON string values, and the two sides are not always consistent.
/// Additionally, format-on-save only updates `new_text` (the replace side) while
/// `old_text` keeps the raw LLM value, which can introduce trailing-newline
/// mismatches.
///
/// We strip one leading `\n` (if present) so both sides start at real content,
/// then ensure both end with exactly one `\n` so `TextDiff` with
/// `newline_terminated(true)` treats the last line consistently.  Interior blank
/// lines (intentional insertions) are preserved.
fn normalize_for_diff(text: &str) -> String {
    let trimmed = text.strip_prefix('\n').unwrap_or(text);
    let trimmed = trimmed.strip_suffix('\n').unwrap_or(trimmed);
    format!("{trimmed}\n")
}

/// One line of a computed unified diff. `text` is a [`SharedString`] so cached
/// diffs can be re-rendered every frame with cheap clones. `emphasis` marks
/// the byte ranges within `text` that changed *within* the line (word diff);
/// they get a stronger background on top of the row color.
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub tag: ChangeTag,
    pub text: SharedString,
    pub emphasis: Vec<std::ops::Range<usize>>,
}

/// Replace blocks with more lines than this (per side) skip the word-level
/// diff — pairing words across big rewrites produces noise, not signal, and
/// the word diff's cost grows with the block. Same cap as Zed.
const MAX_WORD_DIFF_LINES: usize = 8;

/// A line whose emphasized share of non-whitespace bytes exceeds this is
/// mostly rewritten: word emphasis would light up most of it, so its whole
/// replace block is shown as plain changes instead. Long prose paragraphs share enough
/// common words ("the", "data", …) to pass `similar`'s similarity cutoff
/// while every other word changed; this is what filters that out.
const MAX_EMPHASIS_SHARE: f32 = 0.5;

/// Merge emphasis ranges whose gap is whitespace only: word tokens on either
/// side of an unchanged space are one change to the eye.
fn merge_whitespace_gaps(emphasis: &mut Vec<std::ops::Range<usize>>, text: &str) {
    emphasis.dedup_by(|next, prev| {
        let gap = &text[prev.end..next.start];
        if gap.chars().all(char::is_whitespace) {
            prev.end = next.end;
            true
        } else {
            false
        }
    });
}

/// True if emphasizing `emphasis` would cover more than [`MAX_EMPHASIS_SHARE`]
/// of the line's non-whitespace bytes.
fn emphasis_is_noise(emphasis: &[std::ops::Range<usize>], text: &str) -> bool {
    let non_ws = |s: &str| s.bytes().filter(|b| !b.is_ascii_whitespace()).count();
    let total = non_ws(text);
    if total == 0 {
        return false;
    }
    let emphasized: usize = emphasis.iter().map(|r| non_ws(&text[r.clone()])).sum();
    emphasized as f32 / total as f32 > MAX_EMPHASIS_SHARE
}

/// Expand one diff op into [`DiffLine`]s, with word-level emphasis for small
/// replace blocks. `iter_inline_changes` falls back to plain changes on its
/// own when the block's similarity ratio is too low for a useful word diff.
fn collect_change_lines<'a>(
    diff: &'a TextDiff<'a, 'a, 'a, str>,
    op: &similar::DiffOp,
    out: &mut Vec<DiffLine>,
) {
    let block_lines = op.old_range().len().max(op.new_range().len());
    if block_lines <= MAX_WORD_DIFF_LINES {
        let start = out.len();
        let mut noisy = false;
        for change in diff.iter_inline_changes(op) {
            let mut text = String::new();
            let mut emphasis = Vec::new();
            for (emphasized, piece) in change.iter_strings_lossy() {
                let start = text.len();
                text.push_str(&piece);
                if emphasized {
                    emphasis.push(start..text.len());
                }
            }
            let trimmed_len = text.trim_end().len();
            text.truncate(trimmed_len);
            emphasis.retain_mut(|r| {
                r.end = r.end.min(trimmed_len);
                r.start < r.end
            });
            merge_whitespace_gaps(&mut emphasis, &text);
            noisy |= emphasis_is_noise(&emphasis, &text);
            out.push(DiffLine {
                tag: change.tag(),
                text: text.into(),
                emphasis,
            });
        }
        // The word diff pairs both sides of the block, so the noise verdict
        // must too: emphasis on one side with none on the other would suggest
        // a deletion without a counterpart.
        if noisy {
            for line in &mut out[start..] {
                line.emphasis.clear();
            }
        }
    } else {
        for change in diff.iter_changes(op) {
            out.push(DiffLine {
                tag: change.tag(),
                text: change.value().trim_end().to_string().into(),
                emphasis: Vec::new(),
            });
        }
    }
}

/// Run the line diff (the expensive part: normalization + Myers diff + per-line
/// allocations). Callers that render on every frame — like the Review panel —
/// should call this once per content change, cache the result, and feed it to
/// [`render_diff_lines`] per frame.
pub(crate) fn compute_diff_lines(old_text: &str, new_text: &str) -> Vec<DiffLine> {
    let old_norm = normalize_for_diff(old_text);
    let new_norm = normalize_for_diff(new_text);

    let diff = TextDiff::configure()
        .newline_terminated(true)
        .diff_lines(&old_norm, &new_norm);

    let mut lines = Vec::new();
    for op in diff.ops() {
        collect_change_lines(&diff, op, &mut lines);
    }
    lines
}

/// One hunk of a unified diff: a run of changed lines plus surrounding
/// context, positioned at `new_start` (1-based) in the new file.
#[derive(Debug, Clone)]
pub struct DiffHunk {
    pub new_start: usize,
    pub lines: Vec<DiffLine>,
}

/// Like [`compute_diff_lines`], but grouped into hunks with `context` lines
/// of surrounding context (à la `git diff`) — unchanged stretches between
/// hunks are dropped entirely, which keeps the element count proportional to
/// the *changed* lines instead of the file size.
pub fn compute_diff_hunks(old_text: &str, new_text: &str, context: usize) -> Vec<DiffHunk> {
    let old_norm = normalize_for_diff(old_text);
    let new_norm = normalize_for_diff(new_text);

    let diff = TextDiff::configure()
        .newline_terminated(true)
        .diff_lines(&old_norm, &new_norm);

    diff.grouped_ops(context)
        .iter()
        .map(|ops| {
            let mut lines = Vec::new();
            for op in ops {
                collect_change_lines(&diff, op, &mut lines);
            }
            DiffHunk {
                new_start: ops.first().map(|op| op.new_range().start + 1).unwrap_or(1),
                lines,
            }
        })
        .collect()
}

/// A whole file as one one-sided hunk (pure add or pure delete). No diff
/// computation — diffing against an empty side would only produce a phantom
/// deleted/inserted blank line (`normalize_for_diff` maps "" to "\n").
pub fn single_sided_hunk(text: &str, tag: ChangeTag) -> Vec<DiffHunk> {
    let norm = normalize_for_diff(text);
    let lines: Vec<DiffLine> = norm
        .lines()
        .map(|l| DiffLine {
            tag,
            text: l.trim_end().to_string().into(),
            emphasis: Vec::new(),
        })
        .collect();
    if lines.is_empty() {
        return Vec::new();
    }
    vec![DiffHunk {
        new_start: 1,
        lines,
    }]
}

/// Render already-computed hunks with real new-file line numbers, a shared
/// gutter width, and a slim "⋯" separator between hunks.
pub(crate) fn render_diff_hunks(
    hunks: &[DiffHunk],
    theme: &gpui_component::theme::Theme,
    rem_size: gpui::Pixels,
) -> gpui::AnyElement {
    let max_line = hunks
        .iter()
        .map(|h| {
            h.new_start
                + h.lines
                    .iter()
                    .filter(|l| l.tag != ChangeTag::Delete)
                    .count()
        })
        .max()
        .unwrap_or(1);
    let gutter_width = max_line.to_string().len();

    let mut column = div().flex().flex_col();
    for (ix, hunk) in hunks.iter().enumerate() {
        if ix > 0 {
            let (_, ctx_color) = unchanged_row_colors(theme);
            column = column.child(
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .text_color(ctx_color.opacity(0.5))
                    .child("⋯"),
            );
        }
        column = column.child(render_diff_rows(
            &hunk.lines,
            theme,
            Some(hunk.new_start),
            gutter_width,
            rem_size,
        ));
    }
    column.into_any()
}

/// Compute and render a unified diff in one go. For per-frame rendering of
/// unchanged content, prefer caching [`compute_diff_lines`]'s result and
/// calling [`render_diff_lines`] instead.
pub(crate) fn render_unified_diff(
    old_text: &str,
    new_text: &str,
    theme: &gpui_component::theme::Theme,
    start_line: Option<usize>,
    rem_size: gpui::Pixels,
) -> gpui::AnyElement {
    render_diff_lines(
        &compute_diff_lines(old_text, new_text),
        theme,
        start_line,
        rem_size,
    )
}

/// Build the element tree for already-computed diff lines.
pub(crate) fn render_diff_lines(
    diff_lines: &[DiffLine],
    theme: &gpui_component::theme::Theme,
    start_line: Option<usize>,
    rem_size: gpui::Pixels,
) -> gpui::AnyElement {
    // Compute the gutter width (number of digits) based on new-file line numbers
    let gutter_width = if let Some(start) = start_line {
        let new_count = diff_lines
            .iter()
            .filter(|l| l.tag != ChangeTag::Delete)
            .count();
        let max_line = start + new_count;
        max_line.to_string().len()
    } else {
        0
    };
    render_diff_rows(diff_lines, theme, start_line, gutter_width, rem_size)
}

/// Shared row builder: renders diff rows with numbering from `start_line`
/// (when given) into a fixed `gutter_width`-digit gutter.
fn render_diff_rows(
    diff_lines: &[DiffLine],
    theme: &gpui_component::theme::Theme,
    start_line: Option<usize>,
    gutter_width: usize,
    rem_size: gpui::Pixels,
) -> gpui::AnyElement {
    // Track both old and new line numbers
    let mut old_line_num = start_line.unwrap_or(1);
    let mut new_line_num = start_line.unwrap_or(1);

    // Gutter width: compute in rems (~0.5rem per digit + 0.75rem padding),
    // then convert to rounded pixels so it aligns to the pixel grid.
    let gutter_px = rems(gutter_width as f32 * 0.5 + 0.75)
        .to_pixels(rem_size)
        .round();

    div()
        .flex()
        .flex_col()
        .children(diff_lines.iter().map(|dl| {
            let (row_bg, text_color) = match dl.tag {
                ChangeTag::Equal => unchanged_row_colors(theme),
                ChangeTag::Delete => deleted_row_colors(theme),
                ChangeTag::Insert => added_row_colors(theme),
            };

            let mut row = div().w_full().flex().flex_row().items_start();
            if let Some(bg) = row_bg {
                row = row.bg(bg);
            }

            // Gutter with line number (shows new-file line numbers)
            if start_line.is_some() {
                let gutter_text = match dl.tag {
                    ChangeTag::Equal => {
                        let num = new_line_num;
                        old_line_num += 1;
                        new_line_num += 1;
                        format!("{:>width$}", num, width = gutter_width)
                    }
                    ChangeTag::Delete => {
                        old_line_num += 1;
                        format!("{:>width$}", "", width = gutter_width)
                    }
                    ChangeTag::Insert => {
                        let num = new_line_num;
                        new_line_num += 1;
                        format!("{:>width$}", num, width = gutter_width)
                    }
                };
                let gutter_color = match dl.tag {
                    ChangeTag::Equal => unchanged_row_colors(theme).1.opacity(0.5),
                    ChangeTag::Delete => deleted_row_colors(theme).1.opacity(0.5),
                    ChangeTag::Insert => added_row_colors(theme).1.opacity(0.5),
                };
                row = row.child(
                    div()
                        .flex_none()
                        .w(gutter_px)
                        .pl_1p5()
                        .pr_1()
                        .text_color(gutter_color)
                        .child(gutter_text),
                );
            }

            // Content — overflow_x_hidden enables min-width:0 in flex so text
            // wraps instead of pushing the row wider than the card. Word-level
            // changes get a stronger background via text-run highlights, which
            // wrap with the text (unlike per-span elements).
            let content: gpui::AnyElement = if dl.emphasis.is_empty() {
                dl.text.clone().into_any_element()
            } else {
                let word_bg = word_emphasis_bg(dl.tag, theme);
                gpui::StyledText::new(dl.text.clone())
                    .with_highlights(dl.emphasis.iter().map(|range| {
                        (
                            range.clone(),
                            gpui::HighlightStyle {
                                background_color: Some(word_bg),
                                ..Default::default()
                            },
                        )
                    }))
                    .into_any_element()
            };
            row = row.child(
                div()
                    .flex_grow(1.0)
                    .overflow_x_hidden()
                    .when(start_line.is_none(), |d| d.px_3())
                    .when(start_line.is_some(), |d| d.pl_1().pr_3())
                    .text_color(text_color)
                    .child(content),
            );

            row.into_any()
        }))
        .into_any()
}

fn render_streaming_block(
    text: &str,
    is_deletion: bool,
    theme: &gpui_component::theme::Theme,
) -> gpui::AnyElement {
    let (row_bg, text_color) = if is_deletion {
        deleted_row_colors(theme)
    } else {
        added_row_colors(theme)
    };
    let mut row = div()
        .w_full()
        .px_3()
        .text_color(text_color)
        .child(text.to_string());
    if let Some(bg) = row_bg {
        row = row.bg(bg);
    }
    row.into_any()
}

fn render_streaming_diff_section(
    section: &DiffSection,
    theme: &gpui_component::theme::Theme,
) -> gpui::AnyElement {
    let (del_bg, del_text) = deleted_row_colors(theme);
    let (add_bg, add_text) = added_row_colors(theme);
    let mut children: Vec<gpui::AnyElement> = Vec::new();

    if !section.search_content.is_empty() {
        let mut row = div()
            .w_full()
            .px_3()
            .text_color(del_text)
            .child(section.search_content.clone());
        if let Some(bg) = del_bg {
            row = row.bg(bg);
        }
        children.push(row.into_any());
    }
    if !section.replace_content.is_empty() {
        let mut row = div()
            .w_full()
            .px_3()
            .text_color(add_text)
            .child(section.replace_content.clone());
        if let Some(bg) = add_bg {
            row = row.bg(bg);
        }
        children.push(row.into_any());
    }

    div().flex().flex_col().children(children).into_any()
}

// ---------------------------------------------------------------------------
// SEARCH/REPLACE parser
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct DiffSection {
    search_content: String,
    replace_content: String,
    in_search: bool,
    in_replace: bool,
}

fn parse_diff_sections(diff_text: &str) -> Vec<DiffSection> {
    let mut sections = Vec::new();
    let mut current = DiffSection {
        search_content: String::new(),
        replace_content: String::new(),
        in_search: false,
        in_replace: false,
    };

    let normalized = diff_text
        .replace(
            ">>>>>>> REPLACE<<<<<<< SEARCH",
            ">>>>>>> REPLACE\n<<<<<<< SEARCH",
        )
        .replace(
            ">>>>>>> REPLACE_ALL<<<<<<< SEARCH",
            ">>>>>>> REPLACE_ALL\n<<<<<<< SEARCH",
        )
        .replace(
            ">>>>>>> REPLACE<<<<<<< SEARCH_ALL",
            ">>>>>>> REPLACE\n<<<<<<< SEARCH_ALL",
        )
        .replace(
            ">>>>>>> REPLACE_ALL<<<<<<< SEARCH_ALL",
            ">>>>>>> REPLACE_ALL\n<<<<<<< SEARCH_ALL",
        );

    for line in normalized.lines() {
        if line == "<<<<<<< SEARCH" || line == "<<<<<<< SEARCH_ALL" {
            if !current.search_content.is_empty() || !current.replace_content.is_empty() {
                sections.push(current);
                current = DiffSection {
                    search_content: String::new(),
                    replace_content: String::new(),
                    in_search: true,
                    in_replace: false,
                };
            } else {
                current.in_search = true;
                current.in_replace = false;
            }
        } else if line == "=======" {
            current.in_search = false;
            current.in_replace = true;
        } else if line == ">>>>>>> REPLACE" || line == ">>>>>>> REPLACE_ALL" {
            current.in_search = false;
            current.in_replace = false;
            if !current.search_content.is_empty() || !current.replace_content.is_empty() {
                sections.push(current);
                current = DiffSection {
                    search_content: String::new(),
                    replace_content: String::new(),
                    in_search: false,
                    in_replace: false,
                };
            }
        } else if current.in_search {
            if !current.search_content.is_empty() {
                current.search_content.push('\n');
            }
            current.search_content.push_str(line);
        } else if current.in_replace {
            if !current.replace_content.is_empty() {
                current.replace_content.push('\n');
            }
            current.replace_content.push_str(line);
        }
    }

    if !current.search_content.is_empty() || !current.replace_content.is_empty() {
        sections.push(current);
    }
    sections
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn get_param<'a>(tool: &'a ToolUseBlock, name: &str) -> Option<&'a str> {
    tool.parameters
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.value.as_str())
}

/// Check whether a write_file tool's output JSON contains `original_content`,
/// indicating the file was overwritten (not newly created).
fn write_file_has_original_content(tool: &ToolUseBlock) -> bool {
    tool.output
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("original_content").cloned())
        .is_some()
}

/// Extract match start line numbers from the tool's output JSON.
///
/// After execution, `edit` and `replace_in_file` tools emit their output as
/// JSON containing a `match_start_lines` array via `render_for_ui()`.
/// This function attempts to parse that; returns an empty vec on failure.
fn parse_match_start_lines(tool: &ToolUseBlock) -> Vec<usize> {
    tool.output
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("match_start_lines").cloned())
        .and_then(|v| serde_json::from_value::<Vec<usize>>(v).ok())
        .unwrap_or_default()
}

/// Extract path (single) or paths (array) for the header label.
fn extract_path_or_paths(tool: &ToolUseBlock) -> String {
    if let Some(path) = get_param(tool, "path") {
        return path.to_string();
    }
    if let Some(paths_raw) = get_param(tool, "paths") {
        if let Ok(paths) = serde_json::from_str::<Vec<String>>(paths_raw) {
            return paths.join(", ");
        }
        return paths_raw.to_string();
    }
    String::new()
}

fn abbreviate_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if path.starts_with(home_str.as_ref()) {
            return format!("~{}", &path[home_str.len()..]);
        }
    }
    path.to_string()
}

// ---------------------------------------------------------------------------
// Theme colors
// ---------------------------------------------------------------------------

fn rgb_color(r: u8, g: u8, b: u8) -> gpui::Hsla {
    gpui::Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

fn rgba_color(r: u8, g: u8, b: u8, a: u8) -> gpui::Hsla {
    gpui::Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: a as f32 / 255.0,
    }
    .into()
}

pub(crate) fn deleted_row_colors(
    theme: &gpui_component::theme::Theme,
) -> (Option<gpui::Hsla>, gpui::Hsla) {
    if theme.is_dark() {
        (
            Some(rgba_color(0x80, 0x20, 0x20, 0x60)),
            rgb_color(0xFF, 0xBB, 0xBB),
        )
    } else {
        (
            Some(rgba_color(0xDD, 0x55, 0x55, 0x30)),
            rgb_color(0x88, 0x00, 0x00),
        )
    }
}

pub(crate) fn added_row_colors(
    theme: &gpui_component::theme::Theme,
) -> (Option<gpui::Hsla>, gpui::Hsla) {
    if theme.is_dark() {
        (
            Some(rgba_color(0x20, 0x60, 0x20, 0x60)),
            rgb_color(0xBB, 0xFF, 0xBB),
        )
    } else {
        (
            Some(rgba_color(0x33, 0xAA, 0x33, 0x25)),
            rgb_color(0x00, 0x66, 0x00),
        )
    }
}

/// Background for word-level (intra-line) changes: a stronger tint layered on
/// top of the row's add/delete background.
fn word_emphasis_bg(tag: ChangeTag, theme: &gpui_component::theme::Theme) -> gpui::Hsla {
    match (tag, theme.is_dark()) {
        (ChangeTag::Delete, true) => rgba_color(0xC0, 0x38, 0x38, 0x70),
        (ChangeTag::Delete, false) => rgba_color(0xE0, 0x60, 0x60, 0x60),
        (ChangeTag::Insert, true) => rgba_color(0x38, 0xA0, 0x38, 0x70),
        (ChangeTag::Insert, false) => rgba_color(0x40, 0xB8, 0x40, 0x50),
        (ChangeTag::Equal, _) => gpui::transparent_black(),
    }
}

pub(crate) fn unchanged_row_colors(
    theme: &gpui_component::theme::Theme,
) -> (Option<gpui::Hsla>, gpui::Hsla) {
    if theme.is_dark() {
        (None, rgba_color(0xFF, 0xFF, 0xFF, 0x99))
    } else {
        (None, rgba_color(0x33, 0x33, 0x33, 0x99))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_diff_hunks_groups_changes_with_context() {
        let old: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        let mut new_lines: Vec<String> = (1..=20).map(|i| format!("line {i}\n")).collect();
        new_lines[2] = "changed 3\n".into();
        new_lines[15] = "changed 16\n".into();
        let new: String = new_lines.concat();

        let hunks = compute_diff_hunks(&old, &new, 3);
        assert_eq!(hunks.len(), 2, "two distant changes → two hunks");
        assert_eq!(hunks[0].new_start, 1);
        assert_eq!(hunks[1].new_start, 13);
        for hunk in &hunks {
            let deletes = hunk
                .lines
                .iter()
                .filter(|l| l.tag == ChangeTag::Delete)
                .count();
            let inserts = hunk
                .lines
                .iter()
                .filter(|l| l.tag == ChangeTag::Insert)
                .count();
            let equals = hunk
                .lines
                .iter()
                .filter(|l| l.tag == ChangeTag::Equal)
                .count();
            assert_eq!((deletes, inserts), (1, 1));
            assert!(equals <= 6, "at most 3 context lines per side");
        }
    }

    #[test]
    fn compute_diff_lines_marks_word_level_changes() {
        let lines = compute_diff_lines("fn foo(alpha: u32) {}\n", "fn foo(beta: u32) {}\n");
        let del = lines.iter().find(|l| l.tag == ChangeTag::Delete).unwrap();
        let ins = lines.iter().find(|l| l.tag == ChangeTag::Insert).unwrap();

        // The changed identifier is emphasized — not the whole line.
        assert_eq!(del.emphasis.len(), 1);
        assert_eq!(&del.text[del.emphasis[0].clone()], "alpha");
        assert_eq!(ins.emphasis.len(), 1);
        assert_eq!(&ins.text[ins.emphasis[0].clone()], "beta");

        // Unchanged context lines carry no emphasis.
        assert!(
            lines
                .iter()
                .filter(|l| l.tag == ChangeTag::Equal)
                .all(|l| l.emphasis.is_empty())
        );
    }

    #[test]
    fn word_diff_merges_ranges_split_only_by_whitespace() {
        let lines = compute_diff_lines("keep foo bar keep\n", "keep qux quux keep\n");
        let ins = lines.iter().find(|l| l.tag == ChangeTag::Insert).unwrap();
        // "qux" and "quux" are separate word tokens with an unchanged space
        // between them; visually that is one change.
        assert_eq!(ins.emphasis.len(), 1);
        assert_eq!(&ins.text[ins.emphasis[0].clone()], "qux quux");
    }

    #[test]
    fn word_diff_kept_for_small_edit_in_long_paragraph() {
        // A long prose paragraph (one line) with a single changed word is
        // exactly where word emphasis helps most — length must not disable it.
        let filler = "the data center deployment ".repeat(25);
        let old = format!("{filler}serving a jurisdiction.\n");
        let new = format!("{filler}serving one or more jurisdictions.\n");
        assert!(old.len() > 512);
        let lines = compute_diff_lines(&old, &new);
        let ins = lines.iter().find(|l| l.tag == ChangeTag::Insert).unwrap();
        assert_eq!(ins.emphasis.len(), 1);
        assert_eq!(
            &ins.text[ins.emphasis[0].clone()],
            "one or more jurisdictions"
        );
    }

    #[test]
    fn word_diff_noise_decision_covers_both_sides_of_a_block() {
        // The rewritten side crosses the noise threshold, the shorter deleted
        // side does not. Emphasis on one side without a counterpart on the
        // other misleads, so the whole block falls back to plain changes.
        let lines = compute_diff_lines(
            "- **Main Tenant** — AI Core's top-level tenant, mapped one-to-one to a service instance in a BTP subaccount.\n",
            "- **Main Tenant** — AI Core's top-level tenant, identified by the BTP subaccount / zone ID. Multiple AI Core service instances in one subaccount reference the same Main Tenant and Resource Groups.\n",
        );
        let del = lines.iter().find(|l| l.tag == ChangeTag::Delete).unwrap();
        let ins = lines.iter().find(|l| l.tag == ChangeTag::Insert).unwrap();
        assert!(ins.emphasis.is_empty());
        assert!(del.emphasis.is_empty());
    }

    #[test]
    fn word_diff_dropped_when_most_of_the_line_changed() {
        // Enough tokens (spaces, one word) match for `similar` to attempt a
        // word diff, but nearly every word changed: emphasizing most of the
        // line is noise, so the line is shown as a plain change instead.
        let lines = compute_diff_lines(
            "one two three four five six seven\n",
            "uno dos tres four cinco seis siete\n",
        );
        assert!(
            lines
                .iter()
                .filter(|l| l.tag != ChangeTag::Equal)
                .all(|l| l.emphasis.is_empty())
        );
    }

    #[test]
    fn single_sided_hunk_is_one_pure_hunk() {
        let hunks = single_sided_hunk("a\nb\nc\n", ChangeTag::Insert);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].new_start, 1);
        assert!(hunks[0].lines.iter().all(|l| l.tag == ChangeTag::Insert));
        assert_eq!(hunks[0].lines.len(), 3);
    }

    #[test]
    fn test_parse_single_section() {
        let diff = "<<<<<<< SEARCH\nold line\n=======\nnew line\n>>>>>>> REPLACE";
        let sections = parse_diff_sections(diff);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].search_content, "old line");
        assert_eq!(sections[0].replace_content, "new line");
    }

    #[test]
    fn test_parse_multiple_sections() {
        let diff = "<<<<<<< SEARCH\nfirst old\n=======\nfirst new\n>>>>>>> REPLACE\n<<<<<<< SEARCH\nsecond old\n=======\nsecond new\n>>>>>>> REPLACE";
        let sections = parse_diff_sections(diff);
        assert_eq!(sections.len(), 2);
    }

    #[test]
    fn test_parse_streaming_partial() {
        let diff = "<<<<<<< SEARCH\npartial content";
        let sections = parse_diff_sections(diff);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].in_search);
    }

    #[test]
    fn test_parse_concatenated_markers() {
        let diff = "<<<<<<< SEARCH\nold\n=======\nnew\n>>>>>>> REPLACE<<<<<<< SEARCH\nold2\n=======\nnew2\n>>>>>>> REPLACE";
        let sections = parse_diff_sections(diff);
        assert_eq!(sections.len(), 2);
    }

    #[test]
    fn test_extract_path() {
        use crate::blocks::ParameterBlock;
        let tool = ToolUseBlock {
            name: "edit".to_string(),
            id: "test".to_string(),
            parameters: vec![
                ParameterBlock {
                    name: "path".to_string(),
                    value: "src/main.rs".to_string(),
                },
                ParameterBlock {
                    name: "old_text".to_string(),
                    value: "old".to_string(),
                },
            ],

            status: ToolStatus::Success,
            status_message: None,
            output: None,
            styled_output: None,
            state: crate::blocks::ToolBlockState::Collapsed,
            duration_seconds: None,
            images: Vec::new(),
        };
        assert_eq!(extract_path_or_paths(&tool), "src/main.rs");
    }

    #[test]
    fn test_extract_paths_json() {
        use crate::blocks::ParameterBlock;
        let tool = ToolUseBlock {
            name: "delete_files".to_string(),
            id: "test".to_string(),
            parameters: vec![ParameterBlock {
                name: "paths".to_string(),
                value: r#"["a.rs","b.rs"]"#.to_string(),
            }],
            status: ToolStatus::Success,
            status_message: None,
            output: None,
            styled_output: None,
            state: crate::blocks::ToolBlockState::Collapsed,
            duration_seconds: None,
            images: Vec::new(),
        };
        assert_eq!(extract_path_or_paths(&tool), "a.rs, b.rs");
    }

    /// Helper: compute diff tags after the same normalization `render_unified_diff` uses.
    fn diff_tags(old: &str, new: &str) -> Vec<(ChangeTag, String)> {
        let old_norm = normalize_for_diff(old);
        let new_norm = normalize_for_diff(new);
        let diff = TextDiff::configure()
            .newline_terminated(true)
            .diff_lines(&old_norm, &new_norm);
        diff.iter_all_changes()
            .map(|c| (c.tag(), c.value().trim_end().to_string()))
            .collect()
    }

    #[test]
    fn test_diff_no_spurious_leading_green_line() {
        // LLMs sometimes emit new_text with a leading \n that old_text lacks.
        // The normalization should prevent a spurious "added empty line" at top.
        let tags = diff_tags(
            "/// comment\n#[derive(Debug)]",
            "\n/// comment\n#[derive(Debug)]\npub new_field: u32,",
        );
        // First line should be Equal, not Insert
        assert_eq!(
            tags[0].0,
            ChangeTag::Equal,
            "first line should be equal, got {:?}",
            tags
        );
        assert_eq!(tags[0].1, "/// comment");
        // The actually new line should be Insert
        assert!(
            tags.iter()
                .any(|(t, l)| *t == ChangeTag::Insert && l == "pub new_field: u32,")
        );
    }

    #[test]
    fn test_diff_trailing_newline_mismatch_no_spurious_change() {
        // old has no trailing \n, new does — should not produce a spurious change
        let tags = diff_tags("line1\nline2", "line1\nline2\n");
        assert!(
            tags.iter().all(|(t, _)| *t == ChangeTag::Equal),
            "trailing newline mismatch should not produce changes: {:?}",
            tags
        );
    }

    #[test]
    fn test_diff_normal_addition() {
        let tags = diff_tags("line1\nline2\n", "line1\nline2\nnew_line\n");
        assert_eq!(tags.len(), 3);
        assert_eq!(tags[0], (ChangeTag::Equal, "line1".to_string()));
        assert_eq!(tags[1], (ChangeTag::Equal, "line2".to_string()));
        assert_eq!(tags[2], (ChangeTag::Insert, "new_line".to_string()));
    }

    #[test]
    fn test_diff_intentional_blank_line_insertion_preserved() {
        // Adding a blank line between two functions is a real change that must be shown.
        let tags = diff_tags("fn a() {}\nfn b() {}", "fn a() {}\n\nfn b() {}");
        let inserts: Vec<_> = tags
            .iter()
            .filter(|(t, _)| *t == ChangeTag::Insert)
            .collect();
        assert_eq!(inserts.len(), 1, "should insert one blank line: {:?}", tags);
        assert_eq!(inserts[0].1, "", "the inserted line should be blank");
    }

    #[test]
    fn test_diff_leading_newline_mismatch_both_directions() {
        // old has leading \n, new doesn't — should not produce spurious change
        let tags = diff_tags("\nline1\nline2", "line1\nline2\nnew");
        assert_eq!(
            tags[0].0,
            ChangeTag::Equal,
            "first line should be equal: {:?}",
            tags
        );
        assert_eq!(tags[0].1, "line1");
    }
}
