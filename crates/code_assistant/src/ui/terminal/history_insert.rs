use std::io;

use ratatui::{
    backend::Backend,
    buffer::{Buffer, Cell},
    style::Style,
    text::Line,
    Terminal,
};
use unicode_width::UnicodeWidthChar;

fn clear_row(buf: &mut Buffer, width: u16) {
    for x in 0..width {
        if let Some(cell) = buf.cell_mut((x, 0)) {
            *cell = Cell::default();
        }
    }
}

fn write_line(buf: &mut Buffer, width: u16, line: &Line<'_>) {
    let mut x = 0u16;
    let line_style = line.style;

    for span in &line.spans {
        let style = line_style.patch(span.style);
        for ch in span.content.chars() {
            if ch == '\n' || x >= width {
                return;
            }

            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0).max(1) as u16;
            if ch_width == 0 {
                continue;
            }
            if x + ch_width > width {
                return;
            }

            if let Some(cell) = buf.cell_mut((x, 0)) {
                cell.set_style(Style::default().patch(style));
                cell.set_char(ch);
            }

            if ch_width > 1 {
                for pad_x in 1..ch_width {
                    if let Some(cell) = buf.cell_mut((x + pad_x, 0)) {
                        *cell = Cell::default();
                    }
                }
            }

            x += ch_width;
        }
    }
}

pub fn insert_history_lines<B: Backend>(
    terminal: &mut Terminal<B>,
    lines: &[Line<'static>],
) -> io::Result<()> {
    if lines.is_empty() {
        return Ok(());
    }

    let width = terminal.size()?.width;
    for line in lines {
        terminal.insert_before(1, |buf: &mut Buffer| {
            clear_row(buf, width);
            write_line(buf, width, line);
        })?;
    }

    Ok(())
}
