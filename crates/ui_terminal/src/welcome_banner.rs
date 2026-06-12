use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::terminal_color;

/// 5-row bitmap font for each letter in "code".
/// '#' = filled pixel, ' ' = empty. Each letter is rendered at 2x horizontal scale.
/// Top and bottom rows use half-block characters (▄▀) for smooth edges.
fn letter_bitmap(ch: char) -> &'static [&'static str] {
    match ch {
        'c' => &[" ####", "#    ", "#    ", "#    ", " ####"],
        'o' => &[" #### ", "#    #", "#    #", "#    #", " #### "],
        'd' => &["##### ", "#    #", "#    #", "#    #", "##### "],
        'e' => &["######", "#     ", "####  ", "#     ", "######"],
        _ => &["      ", "      ", "      ", "      ", "      "],
    }
}

/// Render "code" as a large block-character banner.
/// Each bitmap pixel becomes 2 characters wide.
/// Top/bottom rows use half-block chars (▄/▀) for smooth edges.
fn render_banner() -> Vec<String> {
    let word = "code";
    let letters: Vec<&[&str]> = word.chars().map(letter_bitmap).collect();
    let letter_spacing = "  ";

    (0..5)
        .map(|row| {
            letters
                .iter()
                .enumerate()
                .map(|(i, letter)| {
                    let prefix = if i > 0 { letter_spacing } else { "" };
                    let expanded: String = letter[row]
                        .chars()
                        .map(|ch| {
                            if ch == '#' {
                                match row {
                                    0 => "▄▄",
                                    4 => "▀▀",
                                    _ => "██",
                                }
                            } else {
                                "  "
                            }
                        })
                        .collect();
                    format!("{prefix}{expanded}")
                })
                .collect()
        })
        .collect()
}

/// Generate styled welcome banner lines for display in terminal scrollback.
pub fn welcome_banner_lines(project_path: &str, is_temporary: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    let accent = banner_accent_color();
    let dim_accent = banner_dim_color();
    let banner_style = Style::default().fg(accent);
    let dim_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);

    // Empty line before banner
    lines.push(Line::from(""));

    // "code" in large block characters
    for row in render_banner() {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(row, banner_style),
        ]));
    }

    // "assistant" subtitle with letter-spacing
    lines.push(Line::from(vec![
        Span::raw("   "),
        Span::styled("a s s i s t a n t", Style::default().fg(dim_accent)),
    ]));

    // Empty line between banner and project info
    lines.push(Line::from(""));

    // Project path
    let mut path_spans = vec![
        Span::raw("   "),
        Span::styled(project_path.to_string(), dim_style),
    ];
    if is_temporary {
        path_spans.push(Span::styled(" (temporary)", dim_style));
    }
    lines.push(Line::from(path_spans));

    // Trailing empty line
    lines.push(Line::from(""));

    lines
}

/// Accent color for the banner, adapts to light/dark terminal backgrounds.
fn banner_accent_color() -> Color {
    match terminal_color::terminal_bg() {
        Some(bg) if is_light(bg) => Color::Rgb(60, 60, 160),
        _ => Color::Rgb(100, 140, 255),
    }
}

/// Dimmer accent for the "assistant" subtitle.
fn banner_dim_color() -> Color {
    match terminal_color::terminal_bg() {
        Some(bg) if is_light(bg) => Color::Rgb(100, 100, 180),
        _ => Color::Rgb(70, 100, 180),
    }
}

fn is_light(bg: (u8, u8, u8)) -> bool {
    let (r, g, b) = bg;
    let y = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    y > 128.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_banner_structure() {
        let lines = welcome_banner_lines("~/projects/test", false);
        // blank + 5 banner + subtitle + blank + path + blank = 10
        assert_eq!(lines.len(), 10);
    }

    #[test]
    fn test_banner_temporary_project() {
        let lines = welcome_banner_lines("~/projects/test", true);
        let path_line = &lines[lines.len() - 2];
        let text: String = path_line
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(text.contains("(temporary)"));
    }

    #[test]
    fn test_banner_configured_project() {
        let lines = welcome_banner_lines("~/projects/test", false);
        let path_line = &lines[lines.len() - 2];
        let text: String = path_line
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(!text.contains("(temporary)"));
        assert!(text.contains("~/projects/test"));
    }

    #[test]
    fn test_banner_rows_consistent_width() {
        let rows = render_banner();
        let widths: Vec<usize> = rows.iter().map(|r| r.chars().count()).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "Banner rows have inconsistent char widths: {:?}",
            widths
        );
    }

    #[test]
    fn test_letter_bitmaps_consistent() {
        for ch in ['c', 'o', 'd', 'e'] {
            let bitmap = letter_bitmap(ch);
            assert_eq!(bitmap.len(), 5, "Letter '{ch}' should have 5 rows");
            let widths: Vec<usize> = bitmap.iter().map(|r| r.chars().count()).collect();
            assert!(
                widths.windows(2).all(|w| w[0] == w[1]),
                "Letter '{ch}' has inconsistent row widths: {:?}",
                widths
            );
        }
    }
}
