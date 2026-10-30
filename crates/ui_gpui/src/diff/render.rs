//! Diff rows as elements: line numbers, syntax colors and word emphasis
//! turned into one [`DiffRows`] element per chunk or snippet, plus the
//! colors diff views share.

use super::chunks::DiffChunk;
use super::model::DiffLine;
use super::prepare::PreparedFileDiff;
use super::rows::{DiffRow, DiffRows, RowGeometry, RowSelection};
use super::syntax::{DiffSyntax, SyntaxOrigin};
use gpui_kit::component::theme::Theme;
use gpui_kit::{
    AnyElement, Element, Hsla, ParentElement, Pixels, SharedString, Styled, div, px, rems,
};
use similar::ChangeTag;

/// Which line numbers the gutter shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GutterNumbers {
    /// New-file numbers; deleted rows leave the gutter blank.
    New,
    /// Old- and new-file numbers side by side, GitHub-style.
    OldAndNew,
}

/// Render one chunk of a prepared file diff with real line numbers,
/// preceded by a slim separator where a later hunk begins (or any hunk
/// with a header, which the separator then shows). With the diff's syntax,
/// rows are highlighted. `selection` makes the rows selectable.
pub(crate) fn render_diff_chunk(
    diff: &PreparedFileDiff,
    chunk: &DiffChunk,
    numbers: GutterNumbers,
    theme: &Theme,
    rem_size: Pixels,
    selection: Option<RowSelection>,
) -> AnyElement {
    let Some(hunk) = diff.hunks.get(chunk.hunk) else {
        return div().into_any();
    };
    let Some(lines) = hunk.lines.get(chunk.lines.clone()) else {
        return div().into_any();
    };
    let syntax = diff.syntax.as_deref().map(|syntax| match syntax.origin() {
        SyntaxOrigin::WholeFile => RowSyntax {
            syntax,
            segment: 0,
            start: LineCounter {
                old: chunk.old_start,
                new: chunk.new_start,
            },
        },
        // The chunk's offset into its hunk, where the hunk's texts start.
        SyntaxOrigin::PerHunk => RowSyntax {
            syntax,
            segment: chunk.hunk,
            start: LineCounter {
                old: chunk.old_start.saturating_sub(hunk.old_start) + 1,
                new: chunk.new_start.saturating_sub(hunk.new_start) + 1,
            },
        },
    });
    let gutter = Gutter {
        numbers,
        start: LineCounter {
            old: chunk.old_start,
            new: chunk.new_start,
        },
        old_width: diff.chunked.old_width,
        new_width: diff.chunked.new_width,
    };
    let rows = render_diff_rows(lines, theme, Some(gutter), rem_size, syntax, selection);
    let header = (chunk.lines.start == 0)
        .then_some(hunk.header.as_ref())
        .flatten();
    if !chunk.starts_later_hunk() && header.is_none() {
        return rows;
    }
    let (_, ctx_color) = unchanged_row_colors(theme);
    let separator = div().w_full().flex().text_color(ctx_color.opacity(0.5));
    let separator = match header {
        Some(header) => separator
            .px_3()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .child(SharedString::from(format!("⋯  {header}"))),
        None => separator.justify_center().child("⋯"),
    };
    div()
        .flex()
        .flex_col()
        .child(separator)
        .child(rows)
        .into_any()
}

/// Build the element tree for already-computed diff lines of a snippet.
/// `start_line` numbers the gutter (new-file numbers); `syntax` was parsed
/// from the snippet alone, so its lines count from 1.
pub(crate) fn render_diff_lines(
    diff_lines: &[DiffLine],
    theme: &Theme,
    start_line: Option<usize>,
    rem_size: Pixels,
    syntax: Option<&DiffSyntax>,
    selection: Option<RowSelection>,
) -> AnyElement {
    let gutter = start_line.map(|start| {
        let new_count = diff_lines
            .iter()
            .filter(|l| l.tag != ChangeTag::Delete)
            .count();
        Gutter {
            numbers: GutterNumbers::New,
            start: LineCounter { old: 1, new: start },
            old_width: 0,
            new_width: (start + new_count).to_string().len(),
        }
    });
    render_diff_rows(
        diff_lines,
        theme,
        gutter,
        rem_size,
        syntax.map(|syntax| RowSyntax {
            syntax,
            segment: 0,
            start: LineCounter::default(),
        }),
        selection,
    )
}

/// 1-based line numbers on both sides of a diff while walking its rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineCounter {
    old: usize,
    new: usize,
}

impl Default for LineCounter {
    fn default() -> Self {
        Self { old: 1, new: 1 }
    }
}

impl LineCounter {
    /// The old and new line number of a row with `tag`, where it has one
    /// (deletions only in the old file, insertions only in the new); then
    /// steps past the row.
    fn step(&mut self, tag: ChangeTag) -> (Option<usize>, Option<usize>) {
        let old = (tag != ChangeTag::Insert).then_some(self.old);
        let new = (tag != ChangeTag::Delete).then_some(self.new);
        self.old += old.is_some() as usize;
        self.new += new.is_some() as usize;
        (old, new)
    }

    /// The line number of a row with `tag` in its own side (the old file for
    /// deletions, the new file otherwise); then steps past the row.
    fn advance(&mut self, tag: ChangeTag) -> usize {
        match self.step(tag) {
            (_, Some(new)) => new,
            (old, None) => old.unwrap_or_default(),
        }
    }
}

/// Syntax source for [`render_diff_rows`]: the parsed text (`segment`) and
/// the line numbers of the first row in it. Those are independent of the
/// gutter's numbers: a tool card parses just its snippet but numbers the
/// gutter by the file, a patch parses each hunk on its own.
struct RowSyntax<'a> {
    syntax: &'a DiffSyntax,
    segment: usize,
    start: LineCounter,
}

/// The line-number column of [`render_diff_rows`]: numbers from `start`, in
/// columns of the given widths (digits).
struct Gutter {
    numbers: GutterNumbers,
    start: LineCounter,
    old_width: usize,
    new_width: usize,
}

impl Gutter {
    /// Width of the gutter text, in characters.
    fn chars(&self) -> usize {
        match self.numbers {
            GutterNumbers::New => self.new_width,
            GutterNumbers::OldAndNew => self.old_width + 1 + self.new_width,
        }
    }

    /// The gutter text of a row with these numbers, padded to [`Self::chars`].
    fn text(&self, old: Option<usize>, new: Option<usize>) -> String {
        let column = |n: Option<usize>, width: usize| match n {
            Some(n) => format!("{n:>width$}"),
            None => " ".repeat(width),
        };
        match self.numbers {
            GutterNumbers::New => column(new, self.new_width),
            GutterNumbers::OldAndNew => format!(
                "{} {}",
                column(old, self.old_width),
                column(new, self.new_width)
            ),
        }
    }
}

/// Shared row builder: the rows with the `gutter` (when given), as one
/// [`DiffRows`] element.
fn render_diff_rows(
    diff_lines: &[DiffLine],
    theme: &Theme,
    gutter: Option<Gutter>,
    rem_size: Pixels,
    syntax: Option<RowSyntax>,
    selection: Option<RowSelection>,
) -> AnyElement {
    let mut gutter_lines = gutter.as_ref().map(|g| g.start).unwrap_or_default();
    let mut syntax_lines = syntax.as_ref().map(|s| s.start).unwrap_or_default();

    // Gutter width: compute in rems (~0.5rem per character + 0.75rem
    // padding), then convert to rounded pixels so it aligns to the pixel grid.
    let gutter_px = gutter.as_ref().map_or(px(0.), |gutter| {
        rems(gutter.chars() as f32 * 0.5 + 0.75)
            .to_pixels(rem_size)
            .round()
    });
    let geometry = RowGeometry {
        gutter_width: gutter_px,
        gutter_left: rems(0.375).to_pixels(rem_size),
        // Without a gutter the text keeps the card's horizontal padding.
        content_left: rems(if gutter.is_some() { 0.25 } else { 0.75 }).to_pixels(rem_size),
        content_right: rems(0.75).to_pixels(rem_size),
    };

    let rows = diff_lines
        .iter()
        .map(|dl| {
            let (row_bg, gutter_fg) = row_colors(dl.tag, theme);
            let (old_num, new_num) = gutter_lines.step(dl.tag);
            let syntax_line = syntax_lines.advance(dl.tag);

            let gutter = gutter.as_ref().map(|gutter| {
                (
                    SharedString::from(gutter.text(old_num, new_num)),
                    gutter_fg.opacity(0.5),
                )
            });

            // Word-level changes get a stronger background via text-run
            // highlights, which wrap with the text. Syntax colors come
            // first, the word emphasis layers on top.
            let syntax_styles = syntax.as_ref().map_or_else(Vec::new, |s| {
                s.syntax.line_styles(
                    dl.tag,
                    s.segment,
                    syntax_line,
                    &dl.text,
                    &theme.highlight_theme,
                )
            });
            let highlights = if dl.emphasis.is_empty() && syntax_styles.is_empty() {
                Vec::new()
            } else {
                let word_bg = word_emphasis_bg(dl.tag, theme);
                let emphasis = dl.emphasis.iter().map(|range| {
                    (
                        range.clone(),
                        gpui_kit::HighlightStyle {
                            background_color: Some(word_bg),
                            ..Default::default()
                        },
                    )
                });
                gpui_kit::combine_highlights(syntax_styles, emphasis).collect()
            };
            DiffRow {
                text: dl.text.clone(),
                highlights,
                background: row_bg,
                color: row_text_color(dl.tag, syntax.is_some(), theme),
                gutter,
            }
        })
        .collect();

    let element = DiffRows::new(rows, geometry);
    match selection {
        Some(selection) => element.selectable(selection).into_any(),
        None => element.into_any(),
    }
}

// ---------------------------------------------------------------------------
// Theme colors
// ---------------------------------------------------------------------------

fn rgb_color(r: u8, g: u8, b: u8) -> Hsla {
    rgba_color(r, g, b, 0xFF)
}

fn rgba_color(r: u8, g: u8, b: u8, a: u8) -> Hsla {
    gpui_kit::Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: a as f32 / 255.0,
    }
    .into()
}

/// Background of a diff body, behind the rows.
pub(crate) fn diff_body_bg(theme: &Theme) -> Hsla {
    if theme.is_dark() {
        gpui_kit::hsla(0.0, 0.0, 0.08, 1.0)
    } else {
        gpui_kit::hsla(0.0, 0.0, 0.97, 1.0)
    }
}

pub(crate) fn deleted_row_colors(theme: &Theme) -> (Option<Hsla>, Hsla) {
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

pub(crate) fn added_row_colors(theme: &Theme) -> (Option<Hsla>, Hsla) {
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

fn unchanged_row_colors(theme: &Theme) -> (Option<Hsla>, Hsla) {
    if theme.is_dark() {
        (None, rgba_color(0xFF, 0xFF, 0xFF, 0x99))
    } else {
        (None, rgba_color(0x33, 0x33, 0x33, 0x99))
    }
}

fn row_colors(tag: ChangeTag, theme: &Theme) -> (Option<Hsla>, Hsla) {
    match tag {
        ChangeTag::Equal => unchanged_row_colors(theme),
        ChangeTag::Delete => deleted_row_colors(theme),
        ChangeTag::Insert => added_row_colors(theme),
    }
}

/// Text color of a row. Syntax highlighted rows use the syntax theme's neutral
/// foreground, since red/green tinted text fights with the syntax colors; the
/// row background still tells the change.
fn row_text_color(tag: ChangeTag, highlighted: bool, theme: &Theme) -> Hsla {
    highlighted
        .then_some(theme.highlight_theme.style.editor_foreground)
        .flatten()
        .unwrap_or_else(|| row_colors(tag, theme).1)
}

/// Background for word-level (intra-line) changes: a stronger tint layered on
/// top of the row's add/delete background.
pub(crate) fn word_emphasis_bg(tag: ChangeTag, theme: &Theme) -> Hsla {
    match (tag, theme.is_dark()) {
        (ChangeTag::Delete, true) => rgba_color(0xC0, 0x38, 0x38, 0x70),
        (ChangeTag::Delete, false) => rgba_color(0xE0, 0x60, 0x60, 0x60),
        (ChangeTag::Insert, true) => rgba_color(0x38, 0xA0, 0x38, 0x70),
        (ChangeTag::Insert, false) => rgba_color(0x40, 0xB8, 0x40, 0x50),
        (ChangeTag::Equal, _) => gpui_kit::transparent_black(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_counter_numbers_each_row_in_its_own_side() {
        let mut lines = LineCounter { old: 10, new: 20 };
        let numbers: Vec<usize> = [
            ChangeTag::Equal,
            ChangeTag::Delete,
            ChangeTag::Delete,
            ChangeTag::Insert,
            ChangeTag::Equal,
        ]
        .into_iter()
        .map(|tag| lines.advance(tag))
        .collect();
        assert_eq!(numbers, [20, 11, 12, 21, 22]);
        assert_eq!(lines, LineCounter { old: 14, new: 23 });
    }

    #[test]
    fn gutter_shows_new_or_both_numbers() {
        let gutter = |numbers| Gutter {
            numbers,
            start: LineCounter::default(),
            old_width: 2,
            new_width: 3,
        };
        let new = gutter(GutterNumbers::New);
        assert_eq!(new.chars(), 3);
        assert_eq!(new.text(Some(9), Some(10)), " 10");
        assert_eq!(new.text(Some(9), None), "   ");

        let both = gutter(GutterNumbers::OldAndNew);
        assert_eq!(both.chars(), 6);
        assert_eq!(both.text(Some(9), Some(10)), " 9  10");
        assert_eq!(both.text(Some(9), None), " 9    ");
        assert_eq!(both.text(None, Some(100)), "   100");
    }

    #[test]
    fn highlighted_rows_use_the_neutral_syntax_foreground() {
        use gpui_kit::component::theme::ThemeColor;
        let mut theme = Theme::from(&*ThemeColor::light());
        theme.highlight_theme = crate::shared::theme::syntax_theme(theme.mode);
        let foreground = theme.highlight_theme.style.editor_foreground.unwrap();

        for tag in [ChangeTag::Equal, ChangeTag::Delete, ChangeTag::Insert] {
            assert_eq!(row_text_color(tag, true, &theme), foreground);
        }
        // Without syntax the rows keep their tinted text.
        assert_eq!(
            row_text_color(ChangeTag::Insert, false, &theme),
            added_row_colors(&theme).1
        );
    }
}
