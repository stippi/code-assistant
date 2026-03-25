use crate::ui::gpui::elements::BlockView;
use crate::ui::gpui::terminal_pool::TerminalPool;
use crate::ui::ToolStatus;
use gpui::AppContext as _; // brings .new() into scope on Context
use gpui::{div, px, App, Context, Entity, IntoElement, ParentElement, Styled, Window};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use terminal::Terminal;
use terminal_view::{TerminalThemeColors, TerminalView};

use super::tool_output_renderers::ToolOutputRenderer;

// ---------------------------------------------------------------------------
// Display-only Terminal Store — fallback for session restoration
// ---------------------------------------------------------------------------

static DISPLAY_TERMINAL_STORE: OnceLock<Mutex<DisplayTerminalStore>> = OnceLock::new();

struct DisplayTerminalStore {
    /// Map from tool_id to a display-only terminal entity
    terminals: HashMap<String, Entity<Terminal>>,
    /// Track the last output length fed into each terminal, to avoid re-feeding
    fed_lengths: HashMap<String, usize>,
}

impl DisplayTerminalStore {
    fn new() -> Self {
        Self {
            terminals: HashMap::new(),
            fed_lengths: HashMap::new(),
        }
    }

    fn global() -> &'static Mutex<DisplayTerminalStore> {
        DISPLAY_TERMINAL_STORE.get_or_init(|| Mutex::new(DisplayTerminalStore::new()))
    }
}

/// Create a display-only terminal and feed text into it (fallback path).
fn get_or_create_display_terminal(tool_id: &str, cx: &mut Context<BlockView>) -> Entity<Terminal> {
    if let Ok(store) = DisplayTerminalStore::global().lock() {
        if let Some(terminal) = store.terminals.get(tool_id) {
            return terminal.clone();
        }
    }

    let builder = terminal::TerminalBuilder::new_display_only(Some(10_000));
    let terminal = cx.new(|cx| builder.subscribe(cx));

    if let Ok(mut store) = DisplayTerminalStore::global().lock() {
        store
            .terminals
            .insert(tool_id.to_string(), terminal.clone());
    }

    terminal
}

/// Feed output text into a display-only terminal. Only feeds new bytes since
/// the last call for this tool_id.
fn feed_display_output(tool_id: &str, output: &str, terminal: &Entity<Terminal>, cx: &mut App) {
    let prev_len = DisplayTerminalStore::global()
        .lock()
        .ok()
        .and_then(|store| store.fed_lengths.get(tool_id).copied())
        .unwrap_or(0);

    if output.len() > prev_len {
        let new_bytes = &output[prev_len..];
        terminal.update(cx, |terminal, cx| {
            terminal.write_output(new_bytes.as_bytes(), cx);
        });
        if let Ok(mut store) = DisplayTerminalStore::global().lock() {
            store.fed_lengths.insert(tool_id.to_string(), output.len());
        }
    }
}

// ---------------------------------------------------------------------------
// View cache — reuse TerminalView entities across re-renders
// ---------------------------------------------------------------------------

static VIEW_CACHE: OnceLock<Mutex<HashMap<String, Entity<TerminalView>>>> = OnceLock::new();

fn view_cache() -> &'static Mutex<HashMap<String, Entity<TerminalView>>> {
    VIEW_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn get_or_create_view(
    cache_key: &str,
    terminal: &Entity<Terminal>,
    theme_colors: TerminalThemeColors,
    cx: &mut Context<BlockView>,
) -> Entity<TerminalView> {
    if let Ok(store) = view_cache().lock() {
        if let Some(view) = store.get(cache_key) {
            return view.clone();
        }
    }

    let terminal_clone = terminal.clone();
    let view = cx.new(|cx| {
        let mut tv = TerminalView::new(terminal_clone, "Menlo", px(13.), theme_colors, cx);
        tv.set_embedded_mode(Some(50), cx);
        tv
    });

    if let Ok(mut store) = view_cache().lock() {
        store.insert(cache_key.to_string(), view.clone());
    }

    view
}

// ---------------------------------------------------------------------------
// ExecuteCommandOutputRenderer
// ---------------------------------------------------------------------------

/// Renders execute_command output as an embedded terminal with ANSI color
/// support.
///
/// **Preferred path**: Looks up a live PTY terminal from the global
/// `TerminalPool` (created by `GpuiTerminalCommandExecutor`).
///
/// **Fallback path**: Creates a display-only terminal and feeds the persisted
/// output text into it (used for session restoration from disk).
///
/// The surrounding tool block already provides header, collapse/expand, and
/// visual framing — this renderer only produces the terminal content itself.
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
        let theme_colors = theme_to_terminal_colors(theme);

        // --- Resolve the Terminal entity ---
        // Try the global pool first (live PTY terminals).
        let terminal = if let Some(entry) = TerminalPool::global().lock().ok().and_then(|pool| {
            pool.get_terminal_by_tool_id_any_session(tool_id)
                .map(|e| e.terminal.clone())
        }) {
            entry
        } else if !output.is_empty() {
            // Fallback: display-only terminal for persisted output.
            let t = get_or_create_display_terminal(tool_id, cx);
            feed_display_output(tool_id, output, &t, cx);
            t
        } else {
            // No live terminal AND no output text — nothing to show.
            return None;
        };

        // --- Get or create the TerminalView ---
        let cache_key = format!("exec-{}", tool_id);
        let view = get_or_create_view(&cache_key, &terminal, theme_colors.clone(), cx);

        // Update theme colors on the view (in case theme changed).
        view.update(cx, |tv, cx| {
            tv.set_theme_colors(theme_colors.clone(), cx);
        });

        // The tool block already provides header, collapse/expand, and
        // border — we just render the terminal content directly.
        Some(
            div()
                .w_full()
                .rounded_md()
                .overflow_hidden()
                .bg(theme_colors.background)
                .child(view)
                .into_any_element(),
        )
    }
}

// ---------------------------------------------------------------------------
// Theme mapping
// ---------------------------------------------------------------------------

fn theme_to_terminal_colors(theme: &gpui_component::theme::Theme) -> TerminalThemeColors {
    if is_dark_theme(theme) {
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
