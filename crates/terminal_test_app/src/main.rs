//! Isolated test app for terminal cards in GPUI.
//!
//! Tests:
//! 1. Real PTY terminals running actual commands
//! 2. Terminal rendering with ANSI colors
//! 3. Attach/detach: switch between terminals, destroy views, reconnect
//!
//! Keyboard shortcuts:
//! - D: Detach all cards (destroy views, terminals keep running)
//! - A: Re-attach cards (create new views for existing terminals)
//! - N: Spawn a new terminal
//! - Cmd+Q: Quit
//!
//! Usage: cargo run --package terminal-test-app

use std::collections::HashMap;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    actions, div, px, rems, size, App, Application, Bounds, Context, Entity, IntoElement,
    KeyBinding, ParentElement, Render, SharedString, Styled, Window, WindowBounds, WindowOptions,
};
use gpui_component::Root;
use terminal::{Terminal, TerminalBuilder, TerminalOptions};
use terminal_view::{TerminalThemeColors, TerminalView};

actions!(
    terminal_test,
    [Quit, DetachCards, AttachCards, SpawnTerminal]
);

// ---------------------------------------------------------------------------
// TerminalPool — owns all terminal entities, independent of views
// ---------------------------------------------------------------------------

struct TerminalPool {
    terminals: HashMap<String, TerminalEntry>,
    next_id: usize,
}

struct TerminalEntry {
    terminal: Entity<Terminal>,
    command: String,
    #[allow(dead_code)]
    started_at: Instant,
}

impl TerminalPool {
    fn new() -> Self {
        Self {
            terminals: HashMap::new(),
            next_id: 0,
        }
    }

    fn spawn(&mut self, command: &str, cx: &mut App) -> String {
        let id = format!("term-{}", self.next_id);
        self.next_id += 1;

        let options = TerminalOptions {
            command: Some(command.to_string()),
            working_dir: None,
            env: vec![("TERM".into(), "xterm-256color".into())],
            scroll_history: Some(10_000),
        };

        let terminal = match TerminalBuilder::new(options) {
            Ok(builder) => cx.new(|cx| builder.subscribe(cx)),
            Err(e) => {
                eprintln!("Failed to create terminal for '{}': {}", command, e);
                return id;
            }
        };

        self.terminals.insert(
            id.clone(),
            TerminalEntry {
                terminal,
                command: command.to_string(),
                started_at: Instant::now(),
            },
        );

        id
    }

    fn get(&self, id: &str) -> Option<&TerminalEntry> {
        self.terminals.get(id)
    }

    fn ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.terminals.keys().cloned().collect();
        ids.sort();
        ids
    }
}

// ---------------------------------------------------------------------------
// TerminalCard — a view that attaches to a terminal from the pool
// ---------------------------------------------------------------------------

struct TerminalCard {
    #[allow(dead_code)]
    terminal_id: String,
    terminal: Entity<Terminal>,
    view: Option<Entity<TerminalView>>,
    command: String,
    theme_colors: TerminalThemeColors,
    _subscriptions: Vec<gpui::Subscription>,
}

impl TerminalCard {
    fn new(
        terminal_id: String,
        terminal: Entity<Terminal>,
        command: String,
        theme_colors: TerminalThemeColors,
        cx: &mut Context<Self>,
    ) -> Self {
        let sub = cx.subscribe(&terminal, |_this, _terminal, _event, cx| {
            cx.notify();
        });

        Self {
            terminal_id,
            terminal,
            view: None,
            command,
            theme_colors,
            _subscriptions: vec![sub],
        }
    }

    fn ensure_view(&mut self, cx: &mut Context<Self>) {
        if self.view.is_some() {
            return;
        }
        let terminal = self.terminal.clone();
        let colors = self.theme_colors.clone();
        let view = cx.new(|cx| {
            let mut tv = TerminalView::new(terminal, "Menlo", rems(0.8125), colors, cx);
            tv.set_embedded_mode(Some(500), cx);
            tv
        });
        self.view = Some(view);
    }
}

impl Render for TerminalCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_view(cx);

        let is_running = !self.terminal.read(cx).has_exited();
        let elapsed = self.terminal.read(cx).started_at().elapsed();
        let exit_status = self.terminal.read(cx).exit_status();

        let status_text = if is_running {
            format!("Running ({:.1}s)", elapsed.as_secs_f64())
        } else if let Some(Some(code)) = exit_status {
            format!("Exited (code {})", code)
        } else {
            "Exited".to_string()
        };

        let border_color = if !is_running {
            if exit_status == Some(Some(0)) {
                gpui::hsla(0.33, 0.5, 0.4, 0.6) // green-ish
            } else {
                gpui::hsla(0.0, 0.6, 0.5, 0.6) // red-ish
            }
        } else {
            gpui::hsla(0.0, 0.0, 0.4, 0.4) // neutral gray
        };

        div()
            .w_full()
            .mb_3()
            .border_1()
            .border_color(border_color)
            .rounded_md()
            .overflow_hidden()
            .child(
                // Header
                div()
                    .px_3()
                    .py_1p5()
                    .bg(gpui::hsla(0.0, 0.0, 0.15, 1.0))
                    .flex()
                    .flex_row()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(gpui::hsla(0.0, 0.0, 0.7, 1.0))
                            .child(format!("$ {}", self.command)),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(gpui::hsla(0.0, 0.0, 0.5, 1.0))
                            .child(status_text),
                    ),
            )
            .child(
                // Terminal body
                div()
                    .w_full()
                    .bg(self.theme_colors.background)
                    .children(self.view.clone()),
            )
    }
}

// ---------------------------------------------------------------------------
// TestApp — root view managing the pool and card views
// ---------------------------------------------------------------------------

struct TestApp {
    pool: TerminalPool,
    /// Currently visible card entities, keyed by terminal_id
    cards: HashMap<String, Entity<TerminalCard>>,
    /// Which terminal is "selected" (highlighted in the sidebar)
    selected_terminal: Option<String>,
    /// Whether cards are currently attached (simulates session connect/disconnect)
    cards_attached: bool,
    theme_colors: TerminalThemeColors,
    focus_handle: gpui::FocusHandle,
}

/// Commands to cycle through for spawning new terminals
const SPAWN_COMMANDS: &[&str] = &[
    "echo 'Hello from a new terminal!' && date && uname -a",
    "ls -la --color=always /tmp",
    "for i in 1 2 3; do echo \"tick $i\"; sleep 1; done && echo 'done'",
    "echo -e '\\033[31mRed\\033[0m \\033[32mGreen\\033[0m \\033[34mBlue\\033[0m \\033[33mYellow\\033[0m'",
    "cat /etc/shells 2>/dev/null || echo 'No /etc/shells'",
];

impl TestApp {
    fn new(cx: &mut Context<Self>) -> Self {
        let theme_colors = dark_theme_colors();
        let mut pool = TerminalPool::new();

        // Spawn a few test terminals with different commands
        pool.spawn("echo '\\033[1;31mRed\\033[0m \\033[1;32mGreen\\033[0m \\033[1;34mBlue\\033[0m' && echo 'Plain text' && ls -la --color=always", cx);
        pool.spawn("for i in 1 2 3 4 5; do echo \"Line $i at $(date +%H:%M:%S)\"; sleep 1; done && echo '\\033[1;33mDone!\\033[0m'", cx);
        pool.spawn("echo '256-color test:' && for i in $(seq 0 255); do printf '\\033[48;5;%dm  \\033[0m' $i; if [ $(( (i + 1) % 16 )) -eq 0 ]; then echo; fi; done", cx);

        let mut app = Self {
            pool,
            cards: HashMap::new(),
            selected_terminal: None,
            cards_attached: true,
            theme_colors,
            focus_handle: cx.focus_handle(),
        };

        // Attach cards for all terminals
        app.attach_all_cards(cx);

        app
    }

    /// Create card views for all terminals in the pool
    fn attach_all_cards(&mut self, cx: &mut Context<Self>) {
        let ids = self.pool.ids();
        for id in ids {
            if self.cards.contains_key(&id) {
                continue;
            }
            if let Some(entry) = self.pool.get(&id) {
                let terminal = entry.terminal.clone();
                let command = entry.command.clone();
                let colors = self.theme_colors.clone();
                let tid = id.clone();
                let card = cx.new(|cx| TerminalCard::new(tid, terminal, command, colors, cx));
                self.cards.insert(id, card);
            }
        }
        if self.selected_terminal.is_none() {
            self.selected_terminal = self.pool.ids().into_iter().next();
        }
    }

    /// Destroy all card views (simulates session disconnect)
    fn detach_all_cards(&mut self, cx: &mut Context<Self>) {
        self.cards.clear();
        self.cards_attached = false;
        cx.notify();
    }

    /// Re-attach cards (simulates session reconnect)
    fn reattach_cards(&mut self, cx: &mut Context<Self>) {
        self.cards_attached = true;
        self.attach_all_cards(cx);
        cx.notify();
    }

    /// Spawn a new terminal with the next command from the cycle
    fn spawn_next_terminal(&mut self, cx: &mut Context<Self>) {
        let cmd_index = self.pool.next_id % SPAWN_COMMANDS.len();
        let command = SPAWN_COMMANDS[cmd_index];
        let id = self.pool.spawn(command, cx);
        if self.cards_attached {
            if let Some(entry) = self.pool.get(&id) {
                let terminal = entry.terminal.clone();
                let cmd = entry.command.clone();
                let colors = self.theme_colors.clone();
                let tid = id.clone();
                let card = cx.new(|cx| TerminalCard::new(tid, terminal, cmd, colors, cx));
                self.cards.insert(id.clone(), card);
            }
        }
        self.selected_terminal = Some(id);
        cx.notify();
    }
}

impl Render for TestApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = gpui::hsla(0.0, 0.0, 0.11, 1.0);
        let sidebar_bg = gpui::hsla(0.0, 0.0, 0.08, 1.0);
        let dim_text = gpui::hsla(0.0, 0.0, 0.4, 1.0);

        div()
            .id("test-app-root")
            .track_focus(&self.focus_handle)
            .key_context("terminal_test")
            .on_action(cx.listener(|this, _: &DetachCards, _window, cx| {
                this.detach_all_cards(cx);
            }))
            .on_action(cx.listener(|this, _: &AttachCards, _window, cx| {
                this.reattach_cards(cx);
            }))
            .on_action(cx.listener(|this, _: &SpawnTerminal, _window, cx| {
                this.spawn_next_terminal(cx);
            }))
            .size_full()
            .bg(bg)
            .text_color(gpui::hsla(0.0, 0.0, 0.85, 1.0))
            .flex()
            .flex_row()
            .child(
                // Sidebar
                div()
                    .w(px(220.0))
                    .h_full()
                    .bg(sidebar_bg)
                    .border_r_1()
                    .border_color(gpui::hsla(0.0, 0.0, 0.2, 1.0))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .text_size(px(14.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Terminal Pool"),
                    )
                    .child(
                        // Terminal list
                        div().flex().flex_col().gap_1().px_2().children(
                            self.pool.ids().into_iter().map(|id| {
                                let entry = self.pool.get(&id).unwrap();
                                let is_selected = self.selected_terminal.as_deref() == Some(&id);
                                let is_running = !entry.terminal.read(cx).has_exited();
                                let has_card = self.cards.contains_key(&id);

                                let indicator = if is_running { "●" } else { "○" };
                                let indicator_color = if is_running {
                                    gpui::hsla(0.33, 0.7, 0.5, 1.0)
                                } else {
                                    gpui::hsla(0.0, 0.0, 0.4, 1.0)
                                };

                                let cmd_display = if entry.command.len() > 20 {
                                    format!("{}...", &entry.command[..20])
                                } else {
                                    entry.command.clone()
                                };

                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded(px(4.0))
                                    .when(is_selected, |d| d.bg(gpui::hsla(0.0, 0.0, 0.2, 1.0)))
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_size(px(10.0))
                                            .text_color(indicator_color)
                                            .child(indicator),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(if has_card {
                                                gpui::hsla(0.0, 0.0, 0.8, 1.0)
                                            } else {
                                                gpui::hsla(0.0, 0.0, 0.4, 1.0)
                                            })
                                            .child(cmd_display),
                                    )
                            }),
                        ),
                    )
                    .child(
                        // Status + key bindings
                        div()
                            .mt_auto()
                            .p_2()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(gpui::hsla(0.0, 0.0, 0.5, 1.0))
                                    .child(format!(
                                        "Cards: {} | Pool: {}",
                                        self.cards.len(),
                                        self.pool.terminals.len()
                                    )),
                            )
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(dim_text)
                                    .child(if self.cards_attached {
                                        "Status: Cards attached"
                                    } else {
                                        "Status: Cards detached"
                                    }),
                            )
                            .child(
                                // Key binding hints
                                div()
                                    .mt_2()
                                    .pt_2()
                                    .border_t_1()
                                    .border_color(gpui::hsla(0.0, 0.0, 0.2, 1.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.0))
                                    .text_size(px(10.0))
                                    .text_color(dim_text)
                                    .child("D - Detach cards")
                                    .child("A - Attach cards")
                                    .child("N - New terminal")
                                    .child("Cmd+Q - Quit"),
                            ),
                    ),
            )
            .child(
                // Main content — terminal cards
                div()
                    .id("terminal-cards-scroll")
                    .flex_1()
                    .h_full()
                    .overflow_y_scroll()
                    .p_4()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(16.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .mb_3()
                            .child("Terminal Cards"),
                    )
                    .children(if self.cards_attached {
                        let ids = self.pool.ids();
                        ids.into_iter()
                            .filter_map(|id| self.cards.get(&id).cloned())
                            .map(|card| div().child(card).into_any_element())
                            .collect::<Vec<_>>()
                    } else {
                        vec![div()
                            .text_size(px(14.0))
                            .text_color(gpui::hsla(0.0, 0.0, 0.4, 1.0))
                            .p_8()
                            .child(
                                "Cards detached — terminals still running in pool. Press A to re-attach.",
                            )
                            .into_any_element()]
                    }),
            )
    }
}

// ---------------------------------------------------------------------------
// Theme colors
// ---------------------------------------------------------------------------

fn dark_theme_colors() -> TerminalThemeColors {
    TerminalThemeColors {
        foreground: gpui::hsla(0.0, 0.0, 0.85, 1.0),
        background: gpui::hsla(0.0, 0.0, 0.1, 1.0),
        cursor: gpui::hsla(0.0, 0.0, 0.85, 1.0),
        ..TerminalThemeColors::default()
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    Application::new().run(move |cx: &mut App| {
        gpui_component::init(cx);
        gpui_component::theme::init(cx);

        // Register key bindings
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("d", DetachCards, Some("terminal_test")),
            KeyBinding::new("a", AttachCards, Some("terminal_test")),
            KeyBinding::new("n", SpawnTerminal, Some("terminal_test")),
        ]);

        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.activate(true);

        let bounds = Bounds::centered(None, size(px(1200.0), px(800.0)), cx);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some(SharedString::from("Terminal Cards Test")),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(TestApp::new);
                // Focus the root view so keyboard shortcuts work
                view.read(cx).focus_handle.focus(window);
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("failed to open window");
    });
}
