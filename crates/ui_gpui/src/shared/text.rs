//! Dynamic text for one-line rows.
//!
//! Rows cut text at the width they have (`.truncate()`, or
//! `.line_clamp(n).text_ellipsis()`), never at a number of characters. A flex
//! item only gets narrower than its text with `min_w_0()` or
//! `overflow_hidden()`, on itself and on any flex item around it.

/// More than any window shows on a line. Keeps a huge tool parameter from
/// being shaped as a whole on every layout just to be cut anyway.
const MAX_SHAPED_CHARS: usize = 1024;

/// The start of `text` that is worth laying out.
pub fn visible_head(text: &str) -> &str {
    text.char_indices()
        .nth(MAX_SHAPED_CHARS)
        .map_or(text, |(ix, _)| &text[..ix])
}

/// `text` on one line: `.truncate()` would keep its line breaks and show
/// several lines.
pub fn single_line(text: &str) -> String {
    visible_head(text)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_breaks_become_spaces() {
        assert_eq!(single_line("  hello  \n\n  world  "), "hello world");
        assert_eq!(single_line("single"), "single");
        assert_eq!(single_line("  \n  \n  "), "");
    }

    #[test]
    fn huge_text_is_limited_on_a_character_boundary() {
        let text = "ä".repeat(MAX_SHAPED_CHARS + 10);
        assert_eq!(single_line(&text).chars().count(), MAX_SHAPED_CHARS);
        let short = "ä".repeat(10);
        assert_eq!(visible_head(&short), short);
    }
}
