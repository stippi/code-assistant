//! Terminal card renderer for `execute_command` tool blocks.
//!
//! Renders the command execution as a bordered card with:
//! - Header: CWD path, elapsed time, stop button (while running)
//! - Body: command line in monospace + embedded TerminalView with ANSI colors
//!
//! This replaces the old `ExecuteCommandOutputRenderer` (ToolOutputRenderer)
//! with a unified `ToolBlockRenderer` that controls the entire card.

use crate::ui::gpui::elements::{BlockView, ToolUseBlock};
use crate::ui::gpui::file_icons;
use crate::ui::gpui::terminal_pool::TerminalPool;
use crate::ui::gpui::tool_block_renderers::{
    animated_card_body, CardRenderContext, ToolBlockRenderer, ToolBlockStyle,
};
use crate::ui::ToolStatus;
use gpui::prelude::FluentBuilder;
use gpui::AppContext as _; // brings .new() into scope on Context
use gpui::{
    div, percentage, px, rems, ClickEvent, Context, Entity, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window,
};
use gpui::{svg, Animation, AnimationExt, Transformation};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use terminal::Terminal;
use terminal_view::{TerminalThemeColors, TerminalView};

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
// TerminalCardRenderer
// ---------------------------------------------------------------------------

/// Renders `execute_command` as a terminal card with header, command line,
/// and embedded terminal output.
pub struct TerminalCardRenderer;

impl ToolBlockRenderer for TerminalCardRenderer {
    fn supported_tools(&self) -> Vec<String> {
        vec!["execute_command".to_string()]
    }

    fn style(&self) -> ToolBlockStyle {
        ToolBlockStyle::Card
    }

    fn describe(&self, tool: &ToolUseBlock) -> String {
        // Extract command_line parameter for description
        if let Some(cmd) = tool.parameters.iter().find(|p| p.name == "command_line") {
            let display = truncate_str(&cmd.value, 60);
            format!("$ {}", display)
        } else {
            "execute command".to_string()
        }
    }

    fn render(
        &self,
        tool: &ToolUseBlock,
        _is_generating: bool,
        theme: &gpui_component::theme::Theme,
        card_ctx: Option<&CardRenderContext>,
        _window: &mut Window,
        cx: &mut Context<BlockView>,
    ) -> Option<gpui::AnyElement> {
        let card_ctx = card_ctx?;
        let theme_colors = theme_to_terminal_colors(theme);

        // Extract parameters
        let command_line_param = tool
            .parameters
            .iter()
            .find(|p| p.name == "command_line")
            .map(|p| p.value.clone())
            .unwrap_or_default();

        let working_dir_param = tool
            .parameters
            .iter()
            .find(|p| p.name == "working_dir")
            .map(|p| p.value.clone());

        let project_param = tool
            .parameters
            .iter()
            .find(|p| p.name == "project")
            .map(|p| p.value.clone())
            .unwrap_or_default();

        let output = tool.output.as_deref().unwrap_or("");

        // --- Resolve live terminal or use static output ---
        let live_terminal = TerminalPool::global().lock().ok().and_then(|pool| {
            pool.get_terminal_by_tool_id_any_session(&tool.id)
                .map(|e| e.terminal.clone())
        });

        // If there's no live terminal and no output, show skeleton or bail —
        // but only when the tool hasn't finished yet. A completed tool with empty
        // output (e.g. `cargo fmt` with no findings) must fall through to the
        // static-output path so the card shows the finished state.
        let tool_finished = matches!(tool.status, ToolStatus::Success | ToolStatus::Error);
        if live_terminal.is_none() && output.is_empty() && !tool_finished {
            if !command_line_param.is_empty() {
                return Some(
                    self.render_skeleton(
                        &tool.id,
                        &command_line_param,
                        working_dir_param.as_deref(),
                        theme,
                    )
                    .into_any_element(),
                );
            }
            return None;
        }

        // Terminal state (only meaningful for live terminals)
        let (is_live, is_running, exit_status, display_command, started_at) =
            if let Some(ref terminal) = live_terminal {
                let t = terminal.read(cx);
                let cmd = if !command_line_param.is_empty() {
                    command_line_param.clone()
                } else {
                    t.command().unwrap_or("command").to_string()
                };
                (true, !t.has_exited(), t.exit_status(), cmd, t.started_at())
            } else {
                let cmd = if !command_line_param.is_empty() {
                    command_line_param.clone()
                } else {
                    "command".to_string()
                };
                (
                    false,
                    false,
                    None,
                    cmd,
                    std::time::Instant::now(), // not used when !is_live
                )
            };

        // Prepare the TerminalView for the live path
        let view = live_terminal.as_ref().map(|terminal| {
            let cache_key = format!("exec-{}", tool.id);
            let v = get_or_create_view(&cache_key, terminal, theme_colors.clone(), cx);
            v.update(cx, |tv, cx| {
                tv.set_theme_colors(theme_colors.clone(), cx);
            });
            v
        });

        let scale = card_ctx.animation_scale;
        let is_collapsed = card_ctx.is_collapsed;

        // --- Build the card ---
        let is_dark = is_dark_theme(theme);
        let has_error = is_card_error(is_running, exit_status, is_live, &tool.status);
        let header_bg = if is_dark {
            gpui::hsla(0.0, 0.0, 0.15, 1.0)
        } else {
            gpui::hsla(0.0, 0.0, 0.93, 1.0)
        };

        let mut card = div()
            .w_full()
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .overflow_hidden();

        // ---- Header ----

        // Elapsed time (while running: shown after 2s; when done: always shown).
        // Once the tool has finished (Success/Error), use the stable duration_seconds
        // from ContentBlock timestamps — this works for both restored sessions and
        // live terminals that have completed (prevents the timer from counting up forever).
        let elapsed_secs = if tool_finished {
            tool.duration_seconds.map(|d| d as u64).or_else(|| {
                if is_live {
                    Some(started_at.elapsed().as_secs())
                } else {
                    None
                }
            })
        } else if is_live {
            Some(started_at.elapsed().as_secs())
        } else {
            None
        };

        // Chevron (right-aligned, matching inline tool style)
        let chevron_icon = if is_collapsed {
            file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN)
        } else {
            file_icons::get().get_type_icon(file_icons::CHEVRON_UP)
        };

        let header_text_color = theme.muted_foreground;

        // Build header: [icon] [command]   [elapsed] [status/✕] [stop] [copy] [▾]
        let mut header_left = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1p5()
            .min_w_0()
            .flex_grow();

        // Terminal icon
        let terminal_icon = file_icons::get().get_tool_icon("execute_command");
        header_left = header_left.child(file_icons::render_icon_container(
            &terminal_icon,
            13.0,
            header_text_color,
            "$",
        ));

        // Command label (always shown) + cross-project indicator
        {
            let mut label_row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .min_w_0()
                .overflow_hidden()
                .child(
                    div()
                        .text_size(rems(0.75))
                        .text_color(header_text_color)
                        .overflow_hidden()
                        .child(truncate_str(&display_command, 200)),
                );

            // Show project name in parentheses if it differs from the session project
            let current_project = &card_ctx.current_project;
            if !project_param.is_empty() && project_param != *current_project {
                label_row = label_row.child(
                    div()
                        .text_size(rems(0.6875))
                        .text_color(header_text_color.opacity(0.5))
                        .flex_shrink_0()
                        .child(format!("({})", project_param)),
                );
            }

            header_left = header_left.child(label_row);
        }

        let mut header_right = div().flex().flex_row().items_center().gap_2();

        if is_running {
            // Running: spinning icon + elapsed time (after 2s)
            let elapsed_str = elapsed_secs.and_then(|s| {
                if s >= 2 {
                    Some(format_elapsed(s))
                } else {
                    None
                }
            });
            header_right = header_right.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(
                        svg()
                            .size(px(12.))
                            .path(SharedString::from("icons/arrow_circle.svg"))
                            .text_color(header_text_color)
                            .with_animation(
                                SharedString::from(format!("term-spin-{}", tool.id)),
                                Animation::new(Duration::from_secs(2)).repeat(),
                                |svg, delta| {
                                    svg.with_transformation(Transformation::rotate(percentage(
                                        delta,
                                    )))
                                },
                            ),
                    )
                    .when_some(elapsed_str, |el, text| {
                        el.child(
                            div()
                                .text_size(rems(0.6875))
                                .text_color(header_text_color.opacity(0.7))
                                .child(text),
                        )
                    }),
            );
        } else if has_error {
            // Error: red ✕ + optional elapsed
            header_right = header_right
                .when_some(elapsed_secs.map(format_elapsed), |el, text| {
                    el.child(
                        div()
                            .text_size(rems(0.6875))
                            .text_color(header_text_color.opacity(0.7))
                            .child(format!("({text})")),
                    )
                })
                .child(
                    gpui::svg()
                        .size(px(13.0))
                        .path(SharedString::from("icons/close.svg"))
                        .text_color(theme.danger),
                );
        } else {
            // Finished successfully: just elapsed time in parentheses
            if let Some(secs) = elapsed_secs {
                header_right = header_right.child(
                    div()
                        .text_size(rems(0.6875))
                        .text_color(header_text_color.opacity(0.7))
                        .child(format!("({})", format_elapsed(secs))),
                );
            }
        }

        // Stop button (only while running)
        if let Some(ref terminal) = live_terminal {
            if is_running {
                let term_for_stop = terminal.clone();
                header_right = header_right.child(
                    div()
                        .id(SharedString::from(format!("stop-{}", tool.id)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(20.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .hover(|s| s.bg(gpui::hsla(0.0, 0.6, 0.5, 0.2)))
                        .child({
                            let stop_icon = file_icons::get().get_type_icon(file_icons::STOP);
                            file_icons::render_icon(
                                &stop_icon,
                                12.0,
                                gpui::hsla(0.0, 0.7, 0.55, 1.0),
                                "■",
                            )
                        })
                        .on_click(move |_event, _window, cx| {
                            cx.stop_propagation();
                            // Send Ctrl-C (ETX) to the PTY to terminate the running process
                            term_for_stop.update(cx, |terminal, _cx| {
                                terminal.write_to_pty(&b"\x03"[..]);
                            });
                        }),
                );
            }
        }

        // Copy button — visible on header hover
        let cmd_for_copy = display_command.clone();
        header_right = header_right.child(
            div()
                .id(SharedString::from(format!("copy-cmd-{}", tool.id)))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .size(px(22.0))
                .rounded(px(4.0))
                .cursor_pointer()
                .opacity(0.0)
                .group_hover("term-header", |s| s.opacity(1.0))
                .hover(|s| s.bg(header_text_color.opacity(0.15)))
                .child(
                    gpui::svg()
                        .size(px(13.0))
                        .path(SharedString::from("icons/copy.svg"))
                        .text_color(header_text_color),
                )
                .on_click(move |_event, _window, cx| {
                    cx.stop_propagation();
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(cmd_for_copy.clone()));
                }),
        );

        // Chevron — highlights on header hover via group
        header_right = header_right.child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded(px(6.))
                .group_hover("term-header", |s| s.bg(header_text_color.opacity(0.1)))
                .child(file_icons::render_icon(
                    &chevron_icon,
                    14.0,
                    header_text_color.opacity(0.4),
                    "▾",
                )),
        );

        card = card.child(
            div()
                .id(SharedString::from(format!("term-header-{}", tool.id)))
                .group("term-header")
                .px_3()
                .py_1p5()
                .bg(header_bg)
                .cursor_pointer()
                .flex()
                .flex_row()
                .justify_between()
                .items_center()
                .map(|d| {
                    if scale <= 0.0 {
                        d.rounded(px(4.))
                    } else {
                        d.rounded_t(px(4.))
                    }
                })
                .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                    view.toggle_tool_collapsed(cx);
                }))
                .child(header_left)
                .child(header_right),
        );

        // ---- Body (animated) ----
        if scale > 0.0 {
            // Output area: live TerminalView or static text
            let output_area = if let Some(view) = view {
                // Live terminal — embedded TerminalView
                div()
                    .w_full()
                    .px_3()
                    .pb_1()
                    .bg(theme_colors.background)
                    .child(view)
            } else {
                // Static output — plain monospace text (no display-only terminal).
                // This avoids resize/reflow instability that caused flickering
                // when using a terminal emulator for static content.
                render_static_output(output, &theme_colors, theme)
            };

            let body_inner = div()
                .flex()
                .flex_col()
                .rounded_b(px(4.))
                .overflow_hidden()
                .child(output_area);

            card = card.child(animated_card_body(
                body_inner,
                scale,
                card_ctx.content_height.clone(),
            ));
        }

        Some(card.into_any_element())
    }
}

impl TerminalCardRenderer {
    /// Render a skeleton card when we have the command but no terminal yet
    /// (e.g. during the brief period before the PTY is created).
    fn render_skeleton(
        &self,
        _tool_id: &str,
        command: &str,
        working_dir: Option<&str>,
        theme: &gpui_component::theme::Theme,
    ) -> gpui::Div {
        let is_dark = is_dark_theme(theme);
        let header_bg = if is_dark {
            gpui::hsla(0.0, 0.0, 0.15, 1.0)
        } else {
            gpui::hsla(0.0, 0.0, 0.93, 1.0)
        };
        let border_color = theme.border;
        let header_text_color = theme.muted_foreground;

        let cwd_text = working_dir.map(abbreviate_path).unwrap_or_default();
        let header_label = if !cwd_text.is_empty() {
            cwd_text
        } else {
            truncate_str(command, 50)
        };

        let terminal_icon = file_icons::get().get_tool_icon("execute_command");

        div()
            .w_full()
            .border_1()
            .border_color(border_color)
            .rounded_md()
            .overflow_hidden()
            // Header
            .child(
                div()
                    .px_3()
                    .py_1p5()
                    .bg(header_bg)
                    .flex()
                    .flex_row()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1p5()
                            .child(file_icons::render_icon_container(
                                &terminal_icon,
                                13.0,
                                header_text_color,
                                "$",
                            ))
                            .child(
                                div()
                                    .text_size(rems(0.75))
                                    .text_color(header_text_color)
                                    .child(header_label),
                            ),
                    )
                    .child(
                        div()
                            .text_size(rems(0.6875))
                            .text_color(theme.muted_foreground)
                            .child("Starting…"),
                    ),
            )
            // Command line
            .child(
                div().px_3().py_1p5().child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .text_size(rems(0.75))
                                .text_color(theme.muted_foreground.opacity(0.6))
                                .child("$"),
                        )
                        .child(
                            div()
                                .text_size(rems(0.78125))
                                .text_color(theme.foreground)
                                .child(truncate_str(command, 80)),
                        ),
                ),
            )
    }
}

// ---------------------------------------------------------------------------
// Card helpers
// ---------------------------------------------------------------------------

/// Maximum lines to show in static output before truncating.
const MAX_STATIC_OUTPUT_LINES: usize = 50;

/// Render command output as static monospace text.
///
/// Used when no live PTY terminal exists (session restore, pool cleanup).
/// Plain text avoids the resize/reflow instability of display-only terminals.
fn render_static_output(
    output: &str,
    theme_colors: &TerminalThemeColors,
    theme: &gpui_component::theme::Theme,
) -> gpui::Div {
    let lines: Vec<&str> = output.trim_end().lines().collect();
    let truncated = lines.len() > MAX_STATIC_OUTPUT_LINES;
    let visible_lines = if truncated {
        &lines[..MAX_STATIC_OUTPUT_LINES]
    } else {
        &lines[..]
    };

    let mut container = div()
        .w_full()
        .px_3()
        .py_1()
        .bg(theme_colors.background)
        .text_size(rems(0.8125))
        .font_family("Menlo")
        .text_color(theme_colors.foreground)
        .overflow_hidden();

    for line in visible_lines {
        container = container.child(div().w_full().child(if line.is_empty() {
            // Empty lines need non-breaking space to maintain line height
            "\u{00a0}".to_string()
        } else {
            line.to_string()
        }));
    }

    if truncated {
        container = container.child(
            div()
                .text_size(rems(0.6875))
                .text_color(theme.muted_foreground.opacity(0.6))
                .pt_1()
                .child(format!(
                    "… {} more lines",
                    lines.len() - MAX_STATIC_OUTPUT_LINES
                )),
        );
    }

    container
}

/// Whether the card represents a failed command.
fn is_card_error(
    is_running: bool,
    exit_status: Option<Option<i32>>,
    is_live: bool,
    status: &ToolStatus,
) -> bool {
    if is_live && !is_running {
        exit_status != Some(Some(0))
    } else {
        matches!(status, ToolStatus::Error)
    }
}

/// Abbreviate a path for display in the header (e.g. replace home dir with ~).
fn abbreviate_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if path.starts_with(home_str.as_ref()) {
            return format!("~{}", &path[home_str.len()..]);
        }
    }
    path.to_string()
}

/// Truncate a string to at most `max_chars` characters, adding "…" if truncated.
fn truncate_str(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count > max_chars {
        let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{}…", truncated)
    } else {
        s.to_string()
    }
}

/// Format elapsed seconds into a human-readable string.
fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
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
