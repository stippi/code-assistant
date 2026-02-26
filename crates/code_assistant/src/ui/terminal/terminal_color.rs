// Terminal background color detection and blending.
// Adapted from codex-rs (https://github.com/openai/codex) under the Apache License 2.0.
//
// Queries the terminal's actual background color via OSC 11 (using the crossterm
// fork's `query_background_color()`) and computes a subtle overlay color for the
// composer input area. On dark terminals the overlay blends white at 12% opacity;
// on light terminals it blends black at 4% opacity.

use ratatui::style::Color;
use std::sync::OnceLock;

/// Cached terminal background color, queried once at startup.
static TERMINAL_BG: OnceLock<Option<(u8, u8, u8)>> = OnceLock::new();

/// Query and cache the terminal's background color.
/// Must be called early (works in both raw and non-raw mode).
pub fn init() {
    TERMINAL_BG.get_or_init(query_terminal_bg);
}

/// Return the cached terminal background color.
pub fn terminal_bg() -> Option<(u8, u8, u8)> {
    *TERMINAL_BG.get_or_init(query_terminal_bg)
}

/// Compute the composer background color based on the terminal's actual background.
/// Returns a subtle overlay: white at 12% on dark, black at 4% on light terminals.
/// Falls back to a reasonable default if the terminal bg couldn't be detected.
pub fn composer_bg() -> Color {
    match terminal_bg() {
        Some(bg) => {
            let (top, alpha) = if is_light(bg) {
                ((0, 0, 0), 0.04)
            } else {
                ((255, 255, 255), 0.12)
            };
            let (r, g, b) = blend(top, bg, alpha);
            Color::Rgb(r, g, b)
        }
        None => Color::Rgb(40, 40, 40), // fallback for terminals that don't support OSC 11
    }
}

/// Compute a subtle background tint for tool content areas (diffs, terminal output).
/// Slightly less prominent than the composer background so it blends more gently.
pub fn tool_content_bg() -> Color {
    match terminal_bg() {
        Some(bg) => {
            let (top, alpha) = if is_light(bg) {
                ((0, 0, 0), 0.03)
            } else {
                ((255, 255, 255), 0.06)
            };
            let (r, g, b) = blend(top, bg, alpha);
            Color::Rgb(r, g, b)
        }
        None => Color::Rgb(35, 35, 35), // fallback for terminals that don't support OSC 11
    }
}

/// Determine if a background color is "light" using ITU-R BT.601 luminance.
fn is_light(bg: (u8, u8, u8)) -> bool {
    let (r, g, b) = bg;
    let y = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    y > 128.0
}

/// Blend `fg` over `bg` at the given alpha (0.0 = fully bg, 1.0 = fully fg).
fn blend(fg: (u8, u8, u8), bg: (u8, u8, u8), alpha: f32) -> (u8, u8, u8) {
    let r = (fg.0 as f32 * alpha + bg.0 as f32 * (1.0 - alpha)) as u8;
    let g = (fg.1 as f32 * alpha + bg.1 as f32 * (1.0 - alpha)) as u8;
    let b = (fg.2 as f32 * alpha + bg.2 as f32 * (1.0 - alpha)) as u8;
    (r, g, b)
}

#[cfg(all(unix, not(test)))]
fn query_terminal_bg() -> Option<(u8, u8, u8)> {
    use crossterm::style::query_background_color;
    use crossterm::style::Color as CrosstermColor;

    match query_background_color() {
        Ok(Some(CrosstermColor::Rgb { r, g, b })) => Some((r, g, b)),
        _ => None,
    }
}

#[cfg(not(all(unix, not(test))))]
fn query_terminal_bg() -> Option<(u8, u8, u8)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_light() {
        assert!(is_light((255, 255, 255))); // white
        assert!(is_light((200, 200, 200))); // light grey
        assert!(!is_light((0, 0, 0))); // black
        assert!(!is_light((30, 30, 30))); // dark grey
        assert!(!is_light((40, 40, 40))); // dark grey
    }

    #[test]
    fn test_blend_dark_bg() {
        // On a dark background (0,0,0), blending white at 12% should give (30,30,30)
        let result = blend((255, 255, 255), (0, 0, 0), 0.12);
        assert_eq!(result, (30, 30, 30));
    }

    #[test]
    fn test_blend_light_bg() {
        // On a light background (255,255,255), blending black at 4% should give (244,244,244)
        let result = blend((0, 0, 0), (255, 255, 255), 0.04);
        assert_eq!(result, (244, 244, 244));
    }

    #[test]
    fn test_blend_typical_dark_terminal() {
        // Typical dark terminal bg like (30, 30, 30)
        let bg = (30, 30, 30);
        assert!(!is_light(bg));
        let result = blend((255, 255, 255), bg, 0.12);
        // Should be slightly lighter than the background
        assert!(result.0 > bg.0);
        assert!(result.1 > bg.1);
        assert!(result.2 > bg.2);
    }
}
