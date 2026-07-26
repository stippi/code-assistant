//! Text helpers shared by the widget renderers.
//!
//! Terminal rendering constantly needs "cut this to N columns". Doing that
//! with byte slices panics on multi-byte input (`&s[..n]` inside a `ü`), so
//! all truncation goes through these helpers.

use std::borrow::Cow;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Longest prefix of `text` that fits into `max_width` terminal columns.
///
/// Cuts on character boundaries and never splits a wide glyph.
pub fn truncate_to_width(text: &str, max_width: usize) -> &str {
    let mut width = 0usize;
    for (idx, ch) in text.char_indices() {
        let ch_width = ch.width().unwrap_or(0);
        if width + ch_width > max_width {
            return &text[..idx];
        }
        width += ch_width;
    }
    text
}

/// Like [`truncate_to_width`], but marks a cut with a trailing `…`.
///
/// The ellipsis is part of the budget, so the result never exceeds
/// `max_width` columns.
pub fn truncate_with_ellipsis(text: &str, max_width: usize) -> Cow<'_, str> {
    if text.width() <= max_width {
        return Cow::Borrowed(text);
    }
    if max_width == 0 {
        return Cow::Borrowed("");
    }
    Cow::Owned(format!("{}…", truncate_to_width(text, max_width - 1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_short_text_untouched() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
    }

    #[test]
    fn cuts_on_char_boundaries() {
        // "Übertragung" — the cut lands inside the multi-byte 'Ü' if done
        // with byte indices.
        assert_eq!(truncate_to_width("Übertragung", 1), "Ü");
        assert_eq!(truncate_to_width("für", 2), "fü");
        assert_eq!(truncate_with_ellipsis("für alle", 3), "fü…");
    }

    #[test]
    fn never_splits_wide_glyphs() {
        // CJK characters occupy two columns each.
        assert_eq!(truncate_to_width("日本語", 3), "日");
        assert_eq!(truncate_to_width("日本語", 4), "日本");
    }

    #[test]
    fn degenerate_widths() {
        assert_eq!(truncate_to_width("abc", 0), "");
        assert_eq!(truncate_with_ellipsis("abc", 0), "");
        assert_eq!(truncate_with_ellipsis("abc", 1), "…");
    }

    #[test]
    fn result_fits_the_budget() {
        for max in 0..12 {
            assert!(truncate_to_width("Fragebogen für Ü", max).width() <= max);
            assert!(truncate_with_ellipsis("Fragebogen für Ü", max).width() <= max);
        }
    }
}
