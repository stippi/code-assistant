use gpui::{rgb, rgba, App};
use gpui_component::theme::{Theme, ThemeMode};

/// Define our custom dark theme colors - matching existing colors
pub fn custom_dark_theme() -> gpui_component::theme::ThemeColor {
    let mut colors = gpui_component::theme::ThemeColor::dark();

    // Main backgrounds
    colors.background = rgb(0x2c2c2c).into(); // Primary background
    colors.card = rgb(0x303030).into(); // Message area background
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

    colors
}

/// Define equivalent light theme colors with good contrast
pub fn custom_light_theme() -> gpui_component::theme::ThemeColor {
    let mut colors = gpui_component::theme::ThemeColor::light();

    // Main backgrounds
    colors.background = rgb(0xF5F5F5).into(); // Light gray background
    colors.card = rgb(0xFFFFFF).into(); // White message area
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

    colors
}

/// Initialize the themes in the app
pub fn init_themes(cx: &mut App) {
    // Register the theme
    gpui_component::theme::init(cx);

    // Get the current theme mode to determine which custom colors to apply
    let theme = cx.global::<Theme>();
    let current_mode = theme.mode;

    // Apply the appropriate custom theme colors based on current mode
    match current_mode {
        ThemeMode::Dark => {
            // Set our custom dark theme colors
            cx.global_mut::<Theme>().colors = custom_dark_theme();
        }
        ThemeMode::Light => {
            // Set our custom light theme colors
            cx.global_mut::<Theme>().colors = custom_light_theme();
        }
    }
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
    use gpui::{black, rgba, white, Hsla};
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

    // Tool block colors
    pub fn tool_block_bg(theme: &Theme) -> Hsla {
        if theme.is_dark() {
            rgba(0x161616FF).into() // Dark mode tool background
        } else {
            rgba(0xF0F0F0FF).into() // Light mode tool background
        }
    }

    pub fn tool_block_icon(theme: &Theme, status: &crate::ui::ToolStatus) -> Hsla {
        // match status {
        //     crate::ui::ToolStatus::Error => theme.warning,
        //     _ => {
        //         if theme.is_dark() {
        //             white()
        //         } else {
        //             black()
        //         }
        //     }
        // }
        match status {
            crate::ui::ToolStatus::Pending => rgba(0x999999FF).into(),
            crate::ui::ToolStatus::Running => theme.info,
            crate::ui::ToolStatus::Success => theme.success,
            crate::ui::ToolStatus::Error => theme.warning,
        }
    }

    pub fn tool_block_name(theme: &Theme, status: &crate::ui::ToolStatus) -> Hsla {
        match status {
            crate::ui::ToolStatus::Error => theme.warning,
            _ => {
                if theme.is_dark() {
                    white()
                } else {
                    black()
                }
            }
        }
    }

    pub fn tool_parameter_bg(theme: &Theme) -> Hsla {
        if theme.is_dark() {
            rgba(0x333333FF).into() // Dark parameter background
        } else {
            rgba(0xE2E2E2FF).into() // Light parameter background
        }
    }

    pub fn tool_parameter_label(theme: &Theme) -> Hsla {
        theme.info // Use theme's info color for parameter labels
    }

    pub fn tool_parameter_value(theme: &Theme) -> Hsla {
        theme.foreground // Use theme's foreground color
    }

    pub fn tool_border_by_status(theme: &Theme, status: &crate::ui::ToolStatus) -> Hsla {
        match status {
            crate::ui::ToolStatus::Pending => rgba(0x999999FF).into(),
            crate::ui::ToolStatus::Running => theme.info,
            crate::ui::ToolStatus::Success => theme.success,
            crate::ui::ToolStatus::Error => theme.warning,
        }
    }
}
