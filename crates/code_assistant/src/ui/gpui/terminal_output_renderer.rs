//! Legacy terminal output renderer — superseded by `terminal_card_renderer.rs`.
//! Kept for reference; will be removed in Phase 4 cleanup.

use crate::ui::gpui::elements::BlockView;
use crate::ui::gpui::terminal_pool::TerminalPool;
use crate::ui::ToolStatus;
use gpui::AppContext as _; // brings .new() into scope on Context
use gpui::{
    div, px, App, Context, Entity, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window,
};
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
// Collapse state — track which tool cards are collapsed
// ---------------------------------------------------------------------------

static COLLAPSED: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();

fn collapsed_state() -> &'static Mutex<HashMap<String, bool>> {
    COLLAPSED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn is_collapsed(tool_id: &str) -> bool {
    collapsed_state()
        .lock()
        .ok()
        .and_then(|m| m.get(tool_id).copied())
        .unwrap_or(false)
}

fn toggle_collapsed(tool_id: &str) {
    if let Ok(mut m) = collapsed_state().lock() {
        let current = m.get(tool_id).copied().unwrap_or(false);
        m.insert(tool_id.to_string(), !current);
    }
}

// ---------------------------------------------------------------------------
// ExecuteCommandOutputRenderer
// ---------------------------------------------------------------------------

/// Renders execute_command output as an embedded terminal card with ANSI
/// color support.
///
/// **Preferred path**: Looks up a live PTY terminal from the global
/// `TerminalPool` (created by `GpuiTerminalCommandExecutor`).
///
/// **Fallback path**: Creates a display-only terminal and feeds the persisted
/// output text into it (used for session restoration from disk).
pub struct ExecuteCommandOutputRenderer;

impl ToolOutputRenderer for ExecuteCommandOutputRenderer {
    fn supported_tools(&self) -> Vec<String> {
        vec!["execute_command".to_string()]
    }

    fn render(
        &self,
        tool_id: &str,
        output: &str,
        status: &ToolStatus,
        theme: &gpui_component::theme::Theme,
        _window: &mut Window,
        cx: &mut Context<BlockView>,
    ) -> Option<gpui::AnyElement> {
        let theme_colors = theme_to_terminal_colors(theme);

        // --- Resolve the Terminal entity ---
        // Try the global pool first (live PTY terminals).
        let (terminal, is_live) = if let Some(entry) =
            TerminalPool::global().lock().ok().and_then(|pool| {
                pool.get_terminal_by_tool_id_any_session(tool_id)
                    .map(|e| e.terminal.clone())
            }) {
            (entry, true)
        } else if !output.is_empty() {
            // Fallback: display-only terminal for persisted output.
            let t = get_or_create_display_terminal(tool_id, cx);
            feed_display_output(tool_id, output, &t, cx);
            (t, false)
        } else {
            // No live terminal AND no output text — nothing to show.
            return None;
        };

        // --- Read terminal state ---
        let (is_running, exit_status, command) = {
            let t = terminal.read(cx);
            (
                !t.has_exited(),
                t.exit_status(),
                t.command().unwrap_or("").to_string(),
            )
        };

        // For display-only terminals the command isn't set; we don't show
        // the card header in that case unless we have info from the status.
        let show_header = is_live || !command.is_empty();

        // --- Get or create the TerminalView ---
        let cache_key = format!("exec-{}", tool_id);
        let view = get_or_create_view(&cache_key, &terminal, theme_colors.clone(), cx);

        // Update theme colors on the view (in case theme changed).
        view.update(cx, |tv, cx| {
            tv.set_theme_colors(theme_colors.clone(), cx);
        });

        let collapsed = is_collapsed(tool_id);

        // --- Build the card element ---
        let border_color = card_border_color(is_running, exit_status, is_live, status);

        let mut card = div()
            .w_full()
            .mt_1()
            .border_1()
            .border_color(border_color)
            .rounded_md()
            .overflow_hidden();

        // Header
        if show_header {
            let status_text = card_status_text(is_running, exit_status, is_live, status);
            let display_command = if command.is_empty() {
                "command".to_string()
            } else {
                command
            };

            let tool_id_for_click = tool_id.to_string();
            let header_bg = if is_dark_theme(theme) {
                gpui::hsla(0.0, 0.0, 0.15, 1.0)
            } else {
                gpui::hsla(0.0, 0.0, 0.93, 1.0)
            };
            let header_text_color = theme.muted_foreground;
            let status_color = theme.muted_foreground;

            // Collapse chevron
            let chevron_icon = if collapsed {
                "icons/chevron_right.svg"
            } else {
                "icons/chevron_down.svg"
            };

            card = card.child(
                div()
                    .id(SharedString::from(format!("term-header-{}", tool_id)))
                    .px_3()
                    .py_1p5()
                    .bg(header_bg)
                    .cursor_pointer()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .items_center()
                    .on_click(move |_event, window, _cx| {
                        toggle_collapsed(&tool_id_for_click);
                        window.refresh();
                    })
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1p5()
                            .child(
                                gpui::svg()
                                    .size(px(12.0))
                                    .path(SharedString::from(chevron_icon))
                                    .text_color(header_text_color),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(header_text_color)
                                    .child(format!("$ {}", display_command)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(status_color)
                            .child(status_text),
                    ),
            );
        }

        // Terminal body (unless collapsed)
        if !collapsed {
            card = card.child(div().w_full().bg(theme_colors.background).child(view));
        }

        Some(card.into_any_element())
    }
}

// ---------------------------------------------------------------------------
// Card helpers
// ---------------------------------------------------------------------------

fn card_border_color(
    is_running: bool,
    exit_status: Option<Option<i32>>,
    is_live: bool,
    status: &ToolStatus,
) -> gpui::Hsla {
    if is_live && is_running {
        // Running — neutral gray
        gpui::hsla(0.0, 0.0, 0.4, 0.4)
    } else if is_live {
        // Live terminal has exited
        if exit_status == Some(Some(0)) {
            gpui::hsla(0.33, 0.5, 0.4, 0.6) // green
        } else {
            gpui::hsla(0.0, 0.6, 0.5, 0.6) // red
        }
    } else {
        // Display-only (restored from persistence) — use tool status

        match status {
            ToolStatus::Pending | ToolStatus::Running => gpui::hsla(0.0, 0.0, 0.4, 0.4),
            ToolStatus::Success => gpui::hsla(0.33, 0.5, 0.4, 0.6),
            ToolStatus::Error => gpui::hsla(0.0, 0.6, 0.5, 0.6),
        }
    }
}

fn card_status_text(
    is_running: bool,
    exit_status: Option<Option<i32>>,
    is_live: bool,
    status: &ToolStatus,
) -> String {
    if is_live {
        if is_running {
            "Running…".to_string()
        } else if let Some(Some(code)) = exit_status {
            if code == 0 {
                "Done".to_string()
            } else {
                format!("Exit {}", code)
            }
        } else {
            "Exited".to_string()
        }
    } else {
        match status {
            ToolStatus::Pending | ToolStatus::Running => "Running…".to_string(),
            ToolStatus::Success => "Done".to_string(),
            ToolStatus::Error => "Error".to_string(),
        }
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
