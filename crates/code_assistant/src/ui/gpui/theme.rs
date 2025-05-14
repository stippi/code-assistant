use gpui::{rgb, rgba, App};
use gpui_component::theme::{Theme, ThemeMode};

/// Define our custom dark theme colors - matching existing colors
pub fn custom_dark_theme() -> gpui_component::theme::ThemeColor {
    let mut colors = gpui_component::theme::ThemeColor::dark();
    
    // Main backgrounds
    colors.background = rgb(0x2c2c2c).into(); // Primary background
    colors.card = rgb(0x303030).into();       // Message area background
    colors.title_bar = rgb(0x303030).into();  // Titlebar background
    colors.title_bar_border = rgb(0x404040).into(); // Titlebar border
    
    // Sidebar
    colors.sidebar = rgb(0x252525).into();    // Sidebar background
    colors.sidebar_border = rgb(0x404040).into(); // Sidebar border
    
    // Text colors
    colors.foreground = rgba(0xFAFAFAFF).into(); // Main text
    colors.muted_foreground = rgb(0xAAAAAA).into(); // Secondary text
    
    // Thinking blocks - blue theme
    colors.info = rgba(0x5BC1FEFF).into();    // Thinking block accent
    colors.info_foreground = rgba(0x93B8CEFF).into(); // Thinking block text
    
    // Buttons
    colors.primary = rgb(0x3355bb).into();    // Primary button (submit)
    colors.primary_hover = rgb(0x4466cc).into();
    colors.danger = rgb(0x553333).into();     // Danger button (clear)
    colors.danger_hover = rgb(0x664444).into();
    
    // Tool status colors
    colors.success = rgb(0x47D136).into();
    colors.warning = rgb(0xFD8E3F).into();
    
    colors
}

/// Define equivalent light theme colors with good contrast
pub fn custom_light_theme() -> gpui_component::theme::ThemeColor {
    let mut colors = gpui_component::theme::ThemeColor::light();
    
    // Main backgrounds
    colors.background = rgb(0xF5F5F5).into(); // Light gray background
    colors.card = rgb(0xFFFFFF).into();       // White message area
    colors.title_bar = rgb(0xE5E5E5).into();  // Light gray titlebar
    colors.title_bar_border = rgb(0xD0D0D0).into(); // Light border
    
    // Sidebar
    colors.sidebar = rgb(0xEAEAEA).into();    // Light sidebar
    colors.sidebar_border = rgb(0xD0D0D0).into(); // Light border
    
    // Text colors
    colors.foreground = rgb(0x333333).into(); // Dark text for contrast
    colors.muted_foreground = rgb(0x777777).into(); // Medium gray text
    
    // Thinking blocks - blue theme (adjusted for light mode)
    colors.info = rgb(0x0085D1).into();       // Thinking block accent
    colors.info_foreground = rgb(0x0060A0).into(); // Thinking block text
    
    // Buttons
    colors.primary = rgb(0x2244AA).into();    // Primary button (submit)
    colors.primary_hover = rgb(0x3355BB).into();
    colors.danger = rgb(0xBB3333).into();     // Danger button (clear)
    colors.danger_hover = rgb(0xCC4444).into();
    
    // Tool status colors
    colors.success = rgb(0x2BB517).into();
    colors.warning = rgb(0xDD7B30).into();
    
    colors
}

/// Initialize the themes in the app
pub fn init_themes(cx: &mut App) {
    // Register the theme
    gpui_component::theme::init(cx);
    
    // Set our custom dark theme colors
    let theme = cx.global_mut::<Theme>();
    theme.colors = custom_dark_theme();
}

/// Toggle between light and dark theme
pub fn toggle_theme(window: Option<&mut gpui::Window>, cx: &mut App) {
    let theme = cx.global::<Theme>();
    let current_mode = theme.mode;
    
    // Toggle to the opposite theme
    match current_mode {
        ThemeMode::Dark => {
            Theme::change(ThemeMode::Light, window, cx);
            // Also update with our custom light theme colors
            cx.global_mut::<Theme>().colors = custom_light_theme();
        }
        ThemeMode::Light => {
            Theme::change(ThemeMode::Dark, window, cx);
            // Also update with our custom dark theme colors 
            cx.global_mut::<Theme>().colors = custom_dark_theme();
        }
    }
}

/// Color utility functions for specific components
pub mod colors {
    use gpui::{rgba, Hsla};
    use gpui_component::theme::Theme;
    
    // Thinking block colors
    pub fn thinking_block_bg(theme: &Theme) -> Hsla {
        if theme.is_dark() {
            rgba(0x00142060).into() // Dark mode blue background
        } else {
            rgba(0x00142020).into() // Light mode blue background
        }
    }
    
    // Tool block background
    pub fn tool_block_bg(theme: &Theme) -> Hsla {
        if theme.is_dark() {
            rgba(0x161616FF).into() // Dark mode tool background
        } else {
            rgba(0xF0F0F0FF).into() // Light mode tool background
        }
    }
}
