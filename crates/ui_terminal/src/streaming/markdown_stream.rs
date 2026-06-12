use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};
use tui_markdown as md;

/// Newline-gated accumulator that renders markdown and commits only fully
/// completed logical lines.
pub struct MarkdownStreamCollector {
    buffer: String,
    committed_line_count: usize,
    width: Option<usize>,
}

impl MarkdownStreamCollector {
    pub fn new(width: Option<usize>) -> Self {
        Self {
            buffer: String::new(),
            committed_line_count: 0,
            width,
        }
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.committed_line_count = 0;
    }

    pub fn set_width(&mut self, width: Option<usize>) {
        self.width = width;
    }

    pub fn push_delta(&mut self, delta: &str) {
        self.buffer.push_str(delta);
    }

    pub fn current_tail(&self) -> &str {
        match self.buffer.rfind('\n') {
            Some(index) => &self.buffer[index + 1..],
            None => &self.buffer,
        }
    }

    /// Render the full buffer and return only newly completed logical lines.
    pub fn commit_complete_lines(&mut self) -> Vec<Line<'static>> {
        let last_newline_idx = match self.buffer.rfind('\n') {
            Some(index) => index,
            None => return Vec::new(),
        };

        let source = &self.buffer[..=last_newline_idx];
        let rendered = render_markdown_lines(source, self.width);
        let mut complete_line_count = rendered.len();

        if complete_line_count > 0 && is_blank_line_spaces_only(&rendered[complete_line_count - 1])
        {
            complete_line_count -= 1;
        }

        if self.committed_line_count >= complete_line_count {
            return Vec::new();
        }

        let out = rendered[self.committed_line_count..complete_line_count].to_vec();
        self.committed_line_count = complete_line_count;
        out
    }

    /// Finalize the stream and emit remaining lines.
    pub fn finalize_and_drain(&mut self) -> Vec<Line<'static>> {
        let mut source = self.buffer.clone();
        if !source.ends_with('\n') {
            source.push('\n');
        }

        let rendered = render_markdown_lines(&source, self.width);
        let mut end = rendered.len();
        // Strip trailing blank lines (consistent with commit_complete_lines)
        while end > self.committed_line_count && is_blank_line_spaces_only(&rendered[end - 1]) {
            end -= 1;
        }

        let out = if self.committed_line_count >= end {
            Vec::new()
        } else {
            rendered[self.committed_line_count..end].to_vec()
        };

        self.clear();
        out
    }
}

pub fn render_markdown_lines(source: &str, width: Option<usize>) -> Vec<Line<'static>> {
    let Some(width) = width.filter(|w| *w > 0) else {
        let text = md::from_str(source);
        let mut lines = text.lines.iter().map(line_to_static).collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(Line::from(""));
        }
        return lines;
    };

    let width = width.min(u16::MAX as usize) as u16;
    let max_height = estimate_render_height(source, width);
    let text = md::from_str(source);
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    let mut tmp = Buffer::empty(Rect::new(0, 0, width, max_height));
    paragraph.render(Rect::new(0, 0, width, max_height), &mut tmp);

    let used_rows = find_used_rows(&tmp, width, max_height);
    let mut lines = Vec::new();
    for y in 0..used_rows {
        let mut spans = Vec::new();
        let mut current_style: Option<Style> = None;
        let mut current_content = String::new();

        for x in 0..width {
            let Some(cell) = tmp.cell((x, y)) else {
                continue;
            };
            let symbol = cell.symbol();
            if symbol.is_empty() {
                continue;
            }

            let style = cell.style();
            if current_style.is_some_and(|existing| existing != style) {
                spans.push(Span::styled(
                    std::mem::take(&mut current_content),
                    current_style.unwrap(),
                ));
                current_style = Some(style);
            } else if current_style.is_none() {
                current_style = Some(style);
            }
            current_content.push_str(symbol);
        }

        if let Some(style) = current_style {
            spans.push(Span::styled(current_content, style));
        }

        if spans.is_empty() {
            lines.push(Line::from(""));
        } else {
            lines.push(Line {
                style: Style::default(),
                alignment: None,
                spans,
            });
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(""));
    }

    lines
}

fn estimate_render_height(source: &str, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }

    let base_lines = source.lines().count().max(1);
    let char_budget = source
        .chars()
        .count()
        .saturating_add(base_lines.saturating_mul(8));
    let rough_wrap = char_budget / width as usize;
    let estimate = rough_wrap.saturating_add(base_lines).saturating_add(16);
    estimate.clamp(16, 8192).min(u16::MAX as usize) as u16
}

fn find_used_rows(buffer: &Buffer, width: u16, max_height: u16) -> u16 {
    for y in (0..max_height).rev() {
        let mut row_empty = true;
        for x in 0..width {
            let Some(cell) = buffer.cell((x, y)) else {
                continue;
            };
            let symbol = cell.symbol();
            if !symbol.is_empty() && symbol != " " {
                row_empty = false;
                break;
            }
        }
        if !row_empty {
            return y + 1;
        }
    }
    0
}

fn line_to_static(line: &Line<'_>) -> Line<'static> {
    Line {
        style: line.style,
        alignment: line.alignment,
        spans: line
            .spans
            .iter()
            .map(|span| Span {
                style: span.style,
                content: std::borrow::Cow::Owned(span.content.to_string()),
            })
            .collect(),
    }
}

fn is_blank_line_spaces_only(line: &Line<'_>) -> bool {
    if line.spans.is_empty() {
        return true;
    }
    line.spans
        .iter()
        .all(|span| span.content.is_empty() || span.content.chars().all(|c| c == ' '))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plain(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn no_commit_until_newline() {
        let mut collector = MarkdownStreamCollector::new(None);
        collector.push_delta("hello");
        assert!(collector.commit_complete_lines().is_empty());

        collector.push_delta(" world\n");
        let lines = collector.commit_complete_lines();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn finalize_commits_partial_line() {
        let mut collector = MarkdownStreamCollector::new(None);
        collector.push_delta("tail");
        let lines = collector.finalize_and_drain();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn width_aware_commit_wraps_like_render_path() {
        let mut collector = MarkdownStreamCollector::new(Some(5));
        collector.push_delta("hello world\n");
        let lines = collector.commit_complete_lines();
        assert!(
            lines.len() >= 2,
            "expected wrapped output, got {:?}",
            lines.iter().map(plain).collect::<Vec<_>>()
        );
    }
}
