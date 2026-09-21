use gpui::{App, Global, rgb, rgba};
use gpui_component::highlighter::{HighlightTheme, HighlightThemeStyle};
use gpui_component::theme::{Theme, ThemeMode};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

/// Define our custom dark theme colors - matching existing colors
pub fn custom_dark_theme() -> gpui_component::theme::ThemeColor {
    let mut colors = *gpui_component::theme::ThemeColor::dark();

    // Main backgrounds
    colors.background = rgb(0x2c2c2c).into(); // Primary background
    colors.popover = rgb(0x303030).into(); // Message area background (was card)
    colors.title_bar = rgb(0x303030).into(); // Titlebar background
    colors.title_bar_border = rgb(0x404040).into(); // Titlebar border

    // Sidebar
    colors.sidebar = rgb(0x252525).into(); // Sidebar background
    colors.sidebar_border = rgb(0x404040).into(); // Sidebar border

    // Text colors
    colors.foreground = rgba(0xFAFAFAFF).into(); // Main text
    colors.muted_foreground = rgb(0xAAAAAA).into(); // Secondary text

    // Thinking blocks - blue theme
    colors.info = rgba(0x5BC1FEFF).into(); // Thinking block accent
    colors.info_foreground = rgba(0x93B8CEFF).into(); // Thinking block text

    // Buttons
    colors.primary = rgb(0x0099EE).into(); // Primary button (submit)
    colors.primary_hover = rgb(0x4466CC).into();
    colors.danger = rgb(0xFF2934).into(); // Danger button (stop)
    colors.danger_hover = rgb(0xFF3D46).into();

    // Tool status colors
    colors.success = rgb(0x47D136).into();
    colors.warning = rgb(0xFD8E3F).into();

    // Accent colors (hover/selection highlights, inline code background)
    colors.accent = rgb(0x353535).into();
    colors.accent_foreground = rgba(0xD6D6D6FF).into();

    colors
}

/// Define equivalent light theme colors with good contrast
pub fn custom_light_theme() -> gpui_component::theme::ThemeColor {
    let mut colors = *gpui_component::theme::ThemeColor::light();

    // Main backgrounds
    colors.background = rgb(0xF5F5F5).into(); // Light gray background
    colors.popover = rgb(0xFFFFFF).into(); // White message area (was card)
    colors.title_bar = rgb(0xE5E5E5).into(); // Light gray titlebar
    colors.title_bar_border = rgb(0xD0D0D0).into(); // Light border

    // Sidebar
    colors.sidebar = rgb(0xEAEAEA).into(); // Light sidebar
    colors.sidebar_border = rgb(0xD0D0D0).into(); // Light border

    // Text colors
    colors.foreground = rgb(0x333333).into(); // Dark text for contrast
    colors.muted_foreground = rgb(0x777777).into(); // Medium gray text

    // Thinking blocks - blue theme (adjusted for light mode)
    colors.info = rgb(0x0085D1).into(); // Thinking block accent
    colors.info_foreground = rgb(0x0060A0).into(); // Thinking block text

    // Buttons
    colors.primary = rgb(0x53AEFF).into(); // Primary button (submit)
    colors.primary_hover = rgb(0x3355BB).into();
    colors.danger = rgb(0xFF2934).into(); // Danger button (stop)
    colors.danger_hover = rgb(0xFF3D46).into();

    // Tool status colors
    colors.success = rgb(0x2BB517).into();
    colors.warning = rgb(0xDD7B30).into();

    // Accent colors (hover/selection highlights, inline code background)
    colors.accent = rgb(0xECECEC).into();
    colors.accent_foreground = rgba(0x333333FF).into();

    colors
}

/// Syntax color sets to choose from; all are tuned for contrast on the
/// diff rows' red/green backgrounds (see the tests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SyntaxVariant {
    /// Few hues: red keywords, blue functions, green strings (after the
    /// GitBook/Shiki theme of the Slate docs).
    #[default]
    Slate,
    /// GitHub-like: blue strings, so nothing but keywords competes with the
    /// diff colors.
    GitHub,
    /// Around our own palette, avoiding red and green altogether.
    Palette,
}

impl SyntaxVariant {
    pub const ALL: [Self; 3] = [Self::Slate, Self::GitHub, Self::Palette];

    pub fn name(self) -> &'static str {
        match self {
            Self::Slate => "slate",
            Self::GitHub => "github",
            Self::Palette => "palette",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.name() == name)
    }

    fn next(self) -> Self {
        let ix = Self::ALL.iter().position(|v| *v == self).unwrap_or(0);
        Self::ALL[(ix + 1) % Self::ALL.len()]
    }
}

struct ActiveSyntaxVariant(SyntaxVariant);

impl Global for ActiveSyntaxVariant {}

/// Environment variable selecting the [`SyntaxVariant`] at startup.
const SYNTAX_THEME_ENV: &str = "CODE_ASSISTANT_SYNTAX_THEME";

static SYNTAX_THEMES: LazyLock<HashMap<(SyntaxVariant, ThemeMode), Arc<HighlightTheme>>> =
    LazyLock::new(|| {
        let styles: HashMap<String, HashMap<ThemeMode, HighlightThemeStyle>> =
            serde_json::from_str(include_str!("syntax_themes.json"))
                .expect("syntax_themes.json is valid");
        SyntaxVariant::ALL
            .into_iter()
            .flat_map(|variant| [(variant, ThemeMode::Light), (variant, ThemeMode::Dark)])
            .map(|(variant, mode)| {
                let theme = HighlightTheme {
                    name: format!("{} {}", variant.name(), mode.name()),
                    appearance: mode,
                    style: styles[variant.name()][&mode].clone(),
                };
                ((variant, mode), Arc::new(theme))
            })
            .collect()
    });

/// The highlight theme of a variant. Always the same `Arc` per variant and
/// mode, so style caches keyed on the theme stay valid.
pub fn syntax_theme(variant: SyntaxVariant, mode: ThemeMode) -> Arc<HighlightTheme> {
    SYNTAX_THEMES[&(variant, mode)].clone()
}

pub fn syntax_variant(cx: &App) -> SyntaxVariant {
    cx.try_global::<ActiveSyntaxVariant>()
        .map(|v| v.0)
        .unwrap_or_default()
}

/// Switch to the next syntax color variant, for comparing them in the app.
pub fn cycle_syntax_variant(cx: &mut App) -> SyntaxVariant {
    let next = syntax_variant(cx).next();
    cx.set_global(ActiveSyntaxVariant(next));
    apply_custom_theme(cx);
    cx.refresh_windows();
    next
}

/// Put our colors and syntax theme over whatever gpui-component set up for
/// the current mode.
fn apply_custom_theme(cx: &mut App) {
    let variant = syntax_variant(cx);
    let theme = cx.global_mut::<Theme>();
    theme.colors = match theme.mode {
        ThemeMode::Dark => custom_dark_theme(),
        ThemeMode::Light => custom_light_theme(),
    };
    theme.highlight_theme = syntax_theme(variant, theme.mode);
}

/// Initialize the themes in the app, optionally restoring a saved mode.
pub fn init_themes(cx: &mut App, mode: Option<ThemeMode>) {
    // Register the theme
    gpui_component::theme::init(cx);

    // If a saved mode was provided, apply it; otherwise use whatever default was set.
    if let Some(mode) = mode {
        Theme::change(mode, None, cx);
    }

    if let Some(variant) = std::env::var(SYNTAX_THEME_ENV)
        .ok()
        .and_then(|name| SyntaxVariant::from_name(&name))
    {
        cx.set_global(ActiveSyntaxVariant(variant));
    }

    apply_custom_theme(cx);
}

/// Toggle between light and dark theme.
///
/// Returns the new [`ThemeMode`] so callers can persist it.
pub fn toggle_theme(window: Option<&mut gpui::Window>, cx: &mut App) -> ThemeMode {
    // Capture the current font_size *before* Theme::change resets it.
    let current_font_size = cx.global::<Theme>().font_size;

    let new_mode = match cx.global::<Theme>().mode {
        ThemeMode::Dark => ThemeMode::Light,
        ThemeMode::Light => ThemeMode::Dark,
    };
    Theme::change(new_mode, window, cx);
    apply_custom_theme(cx);

    // Restore font_size that was clobbered by Theme::change → apply_config.
    cx.global_mut::<Theme>().font_size = current_font_size;

    new_mode
}

/// Color utility functions for specific components
pub mod colors {
    use gpui::{Hsla, rgba};
    use gpui_component::theme::Theme;

    // Thinking block colors
    pub fn thinking_block_bg(theme: &Theme) -> Hsla {
        if theme.is_dark() {
            rgba(0x00142060).into() // Dark mode blue background
        } else {
            rgba(0x00142010).into() // Light mode blue background
        }
    }

    pub fn thinking_block_chevron(theme: &Theme) -> Hsla {
        if theme.is_dark() {
            rgba(0x0099EEFF).into() // Dark mode blue chevron
        } else {
            rgba(0x0077CCFF).into() // Light mode blue chevron
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_cards::diff_card::{
        added_row_colors, deleted_row_colors, diff_body_bg, word_emphasis_bg,
    };
    use gpui::{Hsla, Rgba, TestAppContext};
    use similar::ChangeTag;

    const CAPTURES: &[&str] = &[
        "attribute",
        "boolean",
        "comment",
        "comment.doc",
        "constant",
        "constructor",
        "function",
        "keyword",
        "number",
        "operator",
        "property",
        "punctuation",
        "string",
        "string.escape",
        "string.special",
        "tag",
        "type",
        "variable",
        "variable.special",
    ];

    fn theme_for(mode: ThemeMode) -> Theme {
        let mut theme = Theme::from(&match mode {
            ThemeMode::Dark => custom_dark_theme(),
            ThemeMode::Light => custom_light_theme(),
        });
        theme.mode = mode;
        theme
    }

    fn over(top: Hsla, bottom: Rgba) -> Rgba {
        let top = Rgba::from(top);
        let mix = |t: f32, b: f32| t * top.a + b * (1.0 - top.a);
        Rgba {
            r: mix(top.r, bottom.r),
            g: mix(top.g, bottom.g),
            b: mix(top.b, bottom.b),
            a: 1.0,
        }
    }

    /// WCAG contrast ratio of two opaque colors.
    fn contrast(a: Rgba, b: Rgba) -> f32 {
        let luminance = |c: Rgba| {
            let channel = |v: f32| {
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
        };
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn every_variant_styles_the_core_captures_in_both_modes() {
        for variant in SyntaxVariant::ALL {
            for mode in [ThemeMode::Light, ThemeMode::Dark] {
                let syntax = syntax_theme(variant, mode);
                assert_eq!(syntax.appearance, mode);
                assert!(syntax.style.editor_foreground.is_some());
                for capture in ["keyword", "string", "comment", "function", "type"] {
                    assert!(
                        syntax.style(capture).and_then(|s| s.color).is_some(),
                        "{variant:?}/{mode:?} lacks a color for {capture}"
                    );
                }
            }
        }
    }

    #[test]
    fn syntax_colors_stay_readable_on_diff_rows() {
        for variant in SyntaxVariant::ALL {
            for mode in [ThemeMode::Light, ThemeMode::Dark] {
                let theme = theme_for(mode);
                let syntax = syntax_theme(variant, mode);
                let body = Rgba::from(diff_body_bg(&theme));
                let added = over(added_row_colors(&theme).0.unwrap(), body);
                let deleted = over(deleted_row_colors(&theme).0.unwrap(), body);
                let added_word = over(word_emphasis_bg(ChangeTag::Insert, &theme), added);
                let deleted_word = over(word_emphasis_bg(ChangeTag::Delete, &theme), deleted);

                let colors = CAPTURES
                    .iter()
                    .filter_map(|name| Some((*name, syntax.style(name)?.color?)))
                    .chain([("foreground", syntax.style.editor_foreground.unwrap())]);
                for (name, color) in colors {
                    let color = Rgba::from(color);
                    for (bg_name, bg, min) in [
                        ("body", body, 4.5),
                        ("added row", added, 4.5),
                        ("deleted row", deleted, 4.5),
                        ("added word", added_word, 3.0),
                        ("deleted word", deleted_word, 3.0),
                    ] {
                        let ratio = contrast(color, bg);
                        assert!(
                            ratio >= min,
                            "{variant:?}/{mode:?}: {name} on {bg_name} has contrast {ratio:.2}"
                        );
                    }
                }
            }
        }
    }

    #[gpui::test]
    fn highlight_theme_follows_mode_and_variant(cx: &mut TestAppContext) {
        cx.update(|cx| {
            init_themes(cx, Some(ThemeMode::Dark));
            let variant = syntax_variant(cx);
            assert_eq!(
                cx.global::<Theme>().highlight_theme,
                syntax_theme(variant, ThemeMode::Dark)
            );

            assert_eq!(toggle_theme(None, cx), ThemeMode::Light);
            assert_eq!(
                cx.global::<Theme>().highlight_theme,
                syntax_theme(variant, ThemeMode::Light)
            );

            let next = cycle_syntax_variant(cx);
            assert_ne!(next, variant);
            assert_eq!(
                cx.global::<Theme>().highlight_theme,
                syntax_theme(next, ThemeMode::Light)
            );

            // The variant survives switching the mode.
            toggle_theme(None, cx);
            assert_eq!(
                cx.global::<Theme>().highlight_theme,
                syntax_theme(next, ThemeMode::Dark)
            );
        });
    }
}
