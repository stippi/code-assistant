//! Styled terminal output for static rendering — the gpui-free part of the
//! `terminal` crate's vocabulary. Event payloads that carry captured
//! terminal output (e.g. `UiEvent::UpdateToolStatus`) use these types, so
//! crates that never render a live terminal don't pull in gpui.

/// A line of styled terminal output, composed of one or more spans.
#[derive(Debug, Clone)]
pub struct StyledLine {
    pub spans: Vec<StyledSpan>,
}

impl StyledLine {
    /// Get the plain text of this line (no color info).
    pub fn plain_text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

/// A span of text with a single foreground color and style flags.
///
/// Uses the raw alacritty `Color` enum so that the consumer (e.g. terminal_view)
/// can map it to theme colors at render time.
#[derive(Debug, Clone)]
pub struct StyledSpan {
    pub text: String,
    pub fg: alacritty_terminal::vte::ansi::Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
}

impl StyledSpan {
    /// Whether this span uses the default foreground color.
    pub fn is_default_fg(&self) -> bool {
        matches!(
            self.fg,
            alacritty_terminal::vte::ansi::Color::Named(
                alacritty_terminal::vte::ansi::NamedColor::Foreground
            )
        )
    }
}

/// Trim trailing whitespace-only spans from a line.
pub fn trim_trailing_whitespace(mut spans: Vec<StyledSpan>) -> Vec<StyledSpan> {
    // Trim trailing whitespace from the last span
    while let Some(last) = spans.last_mut() {
        let trimmed = last.text.trim_end();
        if trimmed.is_empty() {
            spans.pop();
        } else {
            last.text = trimmed.to_string();
            break;
        }
    }
    spans
}

/// Map a 256-color index to an RGB value.
/// Indices 0-15 are the standard ANSI colors (caller should map to theme).
/// Indices 16-231 are the 6x6x6 color cube.
/// Indices 232-255 are the grayscale ramp.
pub fn get_indexed_color_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        0..=15 => {
            // Standard ANSI — return black as placeholder, caller should use theme
            (0, 0, 0)
        }
        16..=231 => {
            // 6x6x6 color cube
            let index = index - 16;
            let r = index / 36;
            let g = (index % 36) / 6;
            let b = index % 6;
            let to_val = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            (to_val(r), to_val(g), to_val(b))
        }
        232..=255 => {
            // Grayscale ramp (24 steps)
            let value = 8 + 10 * (index - 232);
            (value, value, value)
        }
    }
}
