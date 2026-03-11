use crate::ui::gpui::elements::BlockView;
use crate::ui::ToolStatus;
use gpui::{div, px, App, AppContext, Context, Entity, IntoElement, ParentElement, Styled, Window};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use terminal::Terminal;
use terminal_view::{TerminalThemeColors, TerminalView};

use super::tool_output_renderers::ToolOutputRenderer;

// ---------------------------------------------------------------------------
// Global Terminal Store — maps tool_id to Terminal entity + TerminalView
// ---------------------------------------------------------------------------

static TERMINAL_STORE: OnceLock<Mutex<TerminalStore>> = OnceLock::new();

struct TerminalStore {
    /// Map from tool_id to a display-only terminal entity
    terminals: HashMap<String, Entity<Terminal>>,
    /// Map from tool_id to a terminal view entity (created lazily during render)
    views: HashMap<String, Entity<TerminalView>>,
    /// Track the last output length fed into each terminal, to avoid re-feeding
    fed_lengths: HashMap<String, usize>,
}

impl TerminalStore {
    fn new() -> Self {
        Self {
            terminals: HashMap::new(),
            views: HashMap::new(),
            fed_lengths: HashMap::new(),
        }
    }

    fn global() -> &'static Mutex<TerminalStore> {
        TERMINAL_STORE.get_or_init(|| Mutex::new(TerminalStore::new()))
    }
}

/// Get the terminal entity for a tool_id, creating one if it doesn't exist.
fn get_or_create_terminal(tool_id: &str, cx: &mut Context<BlockView>) -> Entity<Terminal> {
    // Check if we already have one
    if let Ok(store) = TerminalStore::global().lock() {
        if let Some(terminal) = store.terminals.get(tool_id) {
            return terminal.clone();
        }
    }

    // Create a new display-only terminal
    let builder = terminal::TerminalBuilder::new_display_only(Some(10_000));
    let terminal = cx.new(|cx| builder.subscribe(cx));

    // Store it
    if let Ok(mut store) = TerminalStore::global().lock() {
        store
            .terminals
            .insert(tool_id.to_string(), terminal.clone());
    }

    terminal
}

/// Feed output text into the terminal for a given tool_id.
/// Only feeds new bytes since the last call.
fn feed_output(tool_id: &str, output: &str, terminal: &Entity<Terminal>, cx: &mut App) {
    let prev_len = TerminalStore::global()
        .lock()
        .ok()
        .and_then(|store| store.fed_lengths.get(tool_id).copied())
        .unwrap_or(0);

    if output.len() > prev_len {
        let new_bytes = &output[prev_len..];
        terminal.update(cx, |terminal, cx| {
            terminal.write_output(new_bytes.as_bytes(), cx);
        });
        if let Ok(mut store) = TerminalStore::global().lock() {
            store.fed_lengths.insert(tool_id.to_string(), output.len());
        }
    }
}

/// Get or create a TerminalView for a tool_id.
fn get_or_create_view(
    tool_id: &str,
    terminal: &Entity<Terminal>,
    theme_colors: TerminalThemeColors,
    cx: &mut Context<BlockView>,
) -> Entity<TerminalView> {
    // Check if we already have a view
    if let Ok(store) = TerminalStore::global().lock() {
        if let Some(view) = store.views.get(tool_id) {
            return view.clone();
        }
    }

    // Create a new view
    let terminal_clone = terminal.clone();
    let view = cx.new(|cx| {
        let mut tv = TerminalView::new(terminal_clone, "Berkeley Mono", px(13.), theme_colors, cx);
        tv.set_embedded_mode(Some(1000), cx);
        tv
    });

    // Store it
    if let Ok(mut store) = TerminalStore::global().lock() {
        store.views.insert(tool_id.to_string(), view.clone());
    }

    view
}

/// Remove terminal data for a tool_id (cleanup).
#[allow(dead_code)]
pub fn remove_terminal(tool_id: &str) {
    if let Ok(mut store) = TerminalStore::global().lock() {
        store.terminals.remove(tool_id);
        store.views.remove(tool_id);
        store.fed_lengths.remove(tool_id);
    }
}

// ---------------------------------------------------------------------------
// ExecuteCommandOutputRenderer
// ---------------------------------------------------------------------------

/// Renders execute_command output as an embedded terminal with ANSI color support.
///
/// Instead of showing plain text output, this renderer creates a display-only
/// Terminal entity and feeds the command output into it. The Alacritty terminal
/// emulator processes ANSI escape codes, and the TerminalView renders the
/// resulting styled cell grid.
pub struct ExecuteCommandOutputRenderer;

impl ToolOutputRenderer for ExecuteCommandOutputRenderer {
    fn supported_tools(&self) -> Vec<String> {
        vec!["execute_command".to_string()]
    }

    fn render(
        &self,
        tool_id: &str,
        output: &str,
        _status: &ToolStatus,
        theme: &gpui_component::theme::Theme,
        _window: &mut Window,
        cx: &mut Context<BlockView>,
    ) -> Option<gpui::AnyElement> {
        // Don't render terminal for empty output
        if output.is_empty() {
            return None;
        }

        // Get or create the terminal entity
        let terminal = get_or_create_terminal(tool_id, cx);

        // Feed any new output into the terminal
        feed_output(tool_id, output, &terminal, cx);

        // Get or create the view
        let theme_colors = theme_to_terminal_colors(theme);
        let view = get_or_create_view(tool_id, &terminal, theme_colors, cx);

        let bg_color = theme.background;

        Some(
            div()
                .w_full()
                .rounded_md()
                .overflow_hidden()
                .bg(bg_color)
                .child(view)
                .into_any_element(),
        )
    }
}

// ---------------------------------------------------------------------------
// Theme mapping
// ---------------------------------------------------------------------------

fn theme_to_terminal_colors(theme: &gpui_component::theme::Theme) -> TerminalThemeColors {
    let is_dark = is_dark_theme(theme);

    if is_dark {
        TerminalThemeColors {
            foreground: theme.foreground,
            background: theme.background,
            cursor: theme.foreground,
            ..TerminalThemeColors::default()
        }
    } else {
        // Light theme ANSI colors
        TerminalThemeColors {
            foreground: theme.foreground,
            background: theme.background,
            cursor: theme.foreground,
            ansi_black: rgba(30, 30, 30),
            ansi_red: rgba(194, 24, 7),
            ansi_green: rgba(18, 139, 78),
            ansi_yellow: rgba(183, 149, 0),
            ansi_blue: rgba(0, 82, 163),
            ansi_magenta: rgba(154, 37, 154),
            ansi_cyan: rgba(0, 131, 162),
            ansi_white: rgba(204, 204, 204),
            ansi_bright_black: rgba(102, 102, 102),
            ansi_bright_red: rgba(229, 53, 38),
            ansi_bright_green: rgba(22, 175, 98),
            ansi_bright_yellow: rgba(219, 179, 0),
            ansi_bright_blue: rgba(0, 102, 204),
            ansi_bright_magenta: rgba(188, 63, 188),
            ansi_bright_cyan: rgba(17, 168, 205),
            ansi_bright_white: rgba(229, 229, 229),
        }
    }
}

fn rgba(r: u8, g: u8, b: u8) -> gpui::Hsla {
    gpui::Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

fn is_dark_theme(theme: &gpui_component::theme::Theme) -> bool {
    theme.background.l < 0.5
}
