// History insertion using ANSI scroll regions (DECSTBM).
// Adapted from codex-rs (https://github.com/openai/codex) under the Apache License 2.0.
// Original insert_history.rs is derived from ratatui::Terminal (MIT License).

use std::fmt;
use std::io;
use std::io::Write;

use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::Color as CColor;
use crossterm::style::Colors;
use crossterm::style::Print;
use crossterm::style::SetAttribute;
use crossterm::style::SetBackgroundColor;
use crossterm::style::SetColors;
use crossterm::style::SetForegroundColor;
use crossterm::terminal::Clear;
use crossterm::terminal::ClearType;
use crossterm::Command;
use ratatui::backend::Backend;
use ratatui::layout::Size;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_width::UnicodeWidthChar;

/// Insert `lines` above the viewport using ANSI scroll regions (DECSTBM).
/// This pushes completed content into the native terminal scrollback without
/// disturbing the viewport content below.
pub fn insert_history_lines<B>(
    terminal: &mut crate::ui::terminal::custom_terminal::Terminal<B>,
    lines: Vec<Line>,
) -> io::Result<()>
where
    B: Backend + Write,
{
    let screen_size = terminal.backend().size().unwrap_or(Size::new(0, 0));

    let mut area = terminal.viewport_area;
    let mut should_update_area = false;
    let last_cursor_pos = terminal.last_known_cursor_pos;
    let writer = terminal.backend_mut();

    // Pre-wrap lines so terminal scrollback sees properly formatted text.
    let wrapped = wrap_lines_for_width_styled(&lines, area.width.max(1) as usize);
    let wrapped_lines = wrapped.len() as u16;
    let cursor_top = if area.bottom() < screen_size.height {
        // If the viewport is not at the bottom of the screen, scroll it down to make room.
        let scroll_amount = wrapped_lines.min(screen_size.height - area.bottom());

        // Emit ANSI to scroll the lower region downward by `scroll_amount` lines:
        //   1) Limit the scroll region to [area.top()+1 .. screen_height] (1-based bounds)
        //   2) Place the cursor at the top margin of that region
        //   3) Emit Reverse Index (RI, ESC M) `scroll_amount` times
        //   4) Reset the scroll region back to full screen
        let top_1based = area.top() + 1;
        queue!(writer, SetScrollRegion(top_1based..screen_size.height))?;
        queue!(writer, MoveTo(0, area.top()))?;
        for _ in 0..scroll_amount {
            queue!(writer, Print("\x1bM"))?;
        }
        queue!(writer, ResetScrollRegion)?;

        let cursor_top = area.top().saturating_sub(1);
        area.y += scroll_amount;
        should_update_area = true;
        cursor_top
    } else {
        area.top().saturating_sub(1)
    };

    // Limit the scroll region to the lines from the top of the screen to the
    // top of the viewport. With this in place, when we add lines inside this
    // area, only the lines in this area will be scrolled.
    //
    // ┌─Screen───────────────────────┐
    // │┌╌Scroll region╌╌╌╌╌╌╌╌╌╌╌╌╌╌┐│
    // │┆                            ┆│
    // │┆                            ┆│
    // │█╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌┘│
    // │╭─Viewport───────────────────╮│
    // ││                            ││
    // │╰────────────────────────────╯│
    // └──────────────────────────────┘
    queue!(writer, SetScrollRegion(1..area.top()))?;

    queue!(writer, MoveTo(0, cursor_top))?;

    for line in wrapped {
        queue!(writer, Print("\r\n"))?;
        queue!(
            writer,
            SetColors(Colors::new(
                line.style
                    .fg
                    .map(std::convert::Into::into)
                    .unwrap_or(CColor::Reset),
                line.style
                    .bg
                    .map(std::convert::Into::into)
                    .unwrap_or(CColor::Reset)
            ))
        )?;
        queue!(writer, Clear(ClearType::UntilNewLine))?;
        // Merge line-level style into each span so that ANSI colors reflect
        // line styles (e.g., blockquotes with green fg).
        let merged_spans: Vec<Span> = line
            .spans
            .iter()
            .map(|s| Span {
                style: s.style.patch(line.style),
                content: s.content.clone(),
            })
            .collect();
        write_spans(writer, merged_spans.iter())?;
    }

    queue!(writer, ResetScrollRegion)?;

    // Restore the cursor position to where it was before we started.
    queue!(writer, MoveTo(last_cursor_pos.x, last_cursor_pos.y))?;

    let _ = writer;
    if should_update_area {
        terminal.set_viewport_area(area);
    }

    Ok(())
}

fn write_spans<'a, I>(mut writer: &mut impl Write, content: I) -> io::Result<()>
where
    I: IntoIterator<Item = &'a Span<'a>>,
{
    let mut fg = Color::Reset;
    let mut bg = Color::Reset;
    let mut last_modifier = Modifier::empty();
    for span in content {
        let mut modifier = Modifier::empty();
        modifier.insert(span.style.add_modifier);
        modifier.remove(span.style.sub_modifier);
        if modifier != last_modifier {
            let diff = ModifierDiff {
                from: last_modifier,
                to: modifier,
            };
            diff.queue(&mut writer)?;
            last_modifier = modifier;
        }
        let next_fg = span.style.fg.unwrap_or(Color::Reset);
        let next_bg = span.style.bg.unwrap_or(Color::Reset);
        if next_fg != fg || next_bg != bg {
            queue!(
                writer,
                SetColors(Colors::new(next_fg.into(), next_bg.into()))
            )?;
            fg = next_fg;
            bg = next_bg;
        }

        queue!(writer, Print(span.content.clone()))?;
    }

    queue!(
        writer,
        SetForegroundColor(CColor::Reset),
        SetBackgroundColor(CColor::Reset),
        SetAttribute(crossterm::style::Attribute::Reset),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetScrollRegion(pub std::ops::Range<u16>);

impl Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        panic!("tried to execute SetScrollRegion command using WinAPI, use ANSI instead");
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetScrollRegion;

impl Command for ResetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[r")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        panic!("tried to execute ResetScrollRegion command using WinAPI, use ANSI instead");
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

struct ModifierDiff {
    pub from: Modifier,
    pub to: Modifier,
}

impl ModifierDiff {
    fn queue<W>(self, mut w: W) -> io::Result<()>
    where
        W: io::Write,
    {
        use crossterm::style::Attribute as CAttribute;
        let removed = self.from - self.to;
        if removed.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::NoReverse))?;
        }
        if removed.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
            if self.to.contains(Modifier::DIM) {
                queue!(w, SetAttribute(CAttribute::Dim))?;
            }
        }
        if removed.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::NoItalic))?;
        }
        if removed.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::NoUnderline))?;
        }
        if removed.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
        }
        if removed.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(CAttribute::NotCrossedOut))?;
        }
        if removed.contains(Modifier::SLOW_BLINK) || removed.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(CAttribute::NoBlink))?;
        }

        let added = self.to - self.from;
        if added.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::Reverse))?;
        }
        if added.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::Bold))?;
        }
        if added.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::Italic))?;
        }
        if added.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::Underlined))?;
        }
        if added.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::Dim))?;
        }
        if added.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(CAttribute::CrossedOut))?;
        }
        if added.contains(Modifier::SLOW_BLINK) {
            queue!(w, SetAttribute(CAttribute::SlowBlink))?;
        }
        if added.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(CAttribute::RapidBlink))?;
        }

        Ok(())
    }
}

// --- Line wrapping utilities ---

fn wrap_lines_for_width_styled(lines: &[Line<'_>], width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }

    let mut out = Vec::new();
    for line in lines {
        // Build a list of (char, display_width, span_index) tuples to track
        // which span each character belongs to during wrapping.
        struct CharInfo {
            ch: char,
            display_width: usize,
            span_idx: usize,
        }

        let mut chars: Vec<CharInfo> = Vec::new();
        for (span_idx, span) in line.spans.iter().enumerate() {
            for ch in span.content.chars() {
                chars.push(CharInfo {
                    ch,
                    display_width: UnicodeWidthChar::width(ch).unwrap_or(0),
                    span_idx,
                });
            }
        }

        if chars.is_empty() {
            out.push(Line {
                style: line.style,
                alignment: line.alignment,
                spans: vec![Span::raw(String::new())],
            });
            continue;
        }

        // Walk through chars, splitting into wrapped lines while preserving
        // per-span styles.
        let mut current_spans: Vec<Span<'static>> = Vec::new();
        let mut current_span_text = String::new();
        let mut current_span_idx: Option<usize> = None;
        let mut current_width = 0usize;

        for ci in &chars {
            // Handle embedded newlines: emit current line and start new one
            if ci.ch == '\n' {
                if let Some(idx) = current_span_idx {
                    current_spans.push(Span::styled(
                        std::mem::take(&mut current_span_text),
                        line.spans[idx].style,
                    ));
                }
                out.push(Line {
                    style: line.style,
                    alignment: line.alignment,
                    spans: std::mem::take(&mut current_spans),
                });
                current_span_idx = None;
                current_width = 0;
                continue;
            }

            // Wrap: if adding this char would exceed width, emit the current line
            if ci.display_width > 0 && current_width + ci.display_width > width && current_width > 0
            {
                if let Some(idx) = current_span_idx {
                    current_spans.push(Span::styled(
                        std::mem::take(&mut current_span_text),
                        line.spans[idx].style,
                    ));
                }
                out.push(Line {
                    style: line.style,
                    alignment: line.alignment,
                    spans: std::mem::take(&mut current_spans),
                });
                current_span_idx = None;
                current_width = 0;
            }

            // If the span changed, flush the accumulated span text
            if current_span_idx != Some(ci.span_idx) {
                if let Some(idx) = current_span_idx {
                    current_spans.push(Span::styled(
                        std::mem::take(&mut current_span_text),
                        line.spans[idx].style,
                    ));
                }
                current_span_idx = Some(ci.span_idx);
            }

            current_span_text.push(ci.ch);
            current_width += ci.display_width;
        }

        // Flush remaining
        if let Some(idx) = current_span_idx {
            if !current_span_text.is_empty() {
                current_spans.push(Span::styled(
                    std::mem::take(&mut current_span_text),
                    line.spans[idx].style,
                ));
            }
        }
        if !current_spans.is_empty() {
            out.push(Line {
                style: line.style,
                alignment: line.alignment,
                spans: current_spans,
            });
        } else if current_width == 0 {
            // Empty trailing line (e.g. from trailing newline)
            out.push(Line {
                style: line.style,
                alignment: line.alignment,
                spans: vec![Span::raw(String::new())],
            });
        }
    }
    out
}

#[cfg(test)]
fn line_to_plain(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styled_wrap_preserves_input_line_boundaries() {
        let lines = vec![Line::from("ab"), Line::from("cd")];
        let wrapped = wrap_lines_for_width_styled(&lines, 10);
        let text = wrapped.iter().map(line_to_plain).collect::<Vec<_>>();
        assert_eq!(text, vec!["ab".to_string(), "cd".to_string()]);
    }

    #[test]
    fn styled_wrap_handles_combining_chars_without_column_shift() {
        let lines = vec![Line::from("a\u{0301}bc")];
        let wrapped = wrap_lines_for_width_styled(&lines, 2);
        let text = wrapped.iter().map(line_to_plain).collect::<Vec<_>>();
        assert_eq!(text, vec!["a\u{0301}b".to_string(), "c".to_string()]);
    }
}
