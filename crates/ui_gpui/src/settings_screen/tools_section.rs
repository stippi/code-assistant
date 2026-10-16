//! Built-in tools settings section — per tool group (browser, web, terminal,
//! …) a global on/off switch and whether new sessions start with it on.
//! Backed by `builtin_tools` in `<config_dir>/tools.json`; sessions switch
//! groups in the input row's tools menu.

use code_assistant_core::tools::builtin_selection::{GROUPS, ToolGroup};
use code_assistant_core::tools::config::{BuiltinToolGroupSettings, ToolsConfig};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, Size};
use gpui_kit::{App, Context, FocusHandle, Focusable, SharedString, div, prelude::*, px};
use tracing::warn;

pub struct ToolsSection {
    focus_handle: FocusHandle,
    config: ToolsConfig,
}

fn load_config() -> ToolsConfig {
    // Raw, so saving keeps `${VAR}` references intact.
    ToolsConfig::load_raw().unwrap_or_else(|e| {
        warn!("Failed to load tools.json: {e:#}");
        ToolsConfig::default()
    })
}

impl ToolsSection {
    pub fn new(_window: &mut gpui_kit::Window, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            config: load_config(),
        }
    }

    /// Reload the configuration from disk.
    pub fn reload(&mut self) {
        self.config = load_config();
    }

    fn update_group(
        &mut self,
        id: &str,
        change: impl FnOnce(&mut BuiltinToolGroupSettings),
        cx: &mut Context<Self>,
    ) {
        let mut settings = self.config.builtin_group(id);
        change(&mut settings);
        // Drop entries back at the defaults so tools.json stays minimal.
        if settings == BuiltinToolGroupSettings::default() {
            self.config.builtin_tools.remove(id);
        } else {
            self.config.builtin_tools.insert(id.to_string(), settings);
        }
        if let Err(e) = self.config.save() {
            warn!("Failed to save tools.json: {e:#}");
        }
        cx.notify();
    }

    fn render_group(&self, group: &ToolGroup, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.config.builtin_group(group.id);
        let view = cx.entity();
        let id_for_enabled = group.id;
        let id_for_new_sessions = group.id;

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .py_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(cx.theme().foreground)
                                    .child(group.label),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(group.description),
                            ),
                    )
                    .child(
                        Switch::new(SharedString::from(format!("builtin-enabled-{}", group.id)))
                            .checked(settings.enabled)
                            .with_size(Size::Small)
                            .on_click({
                                let view = view.clone();
                                move |on, _window, app| {
                                    let on = *on;
                                    view.update(app, |this, cx| {
                                        this.update_group(id_for_enabled, |s| s.enabled = on, cx);
                                    });
                                }
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Switch::new(SharedString::from(format!(
                            "builtin-new-sessions-{}",
                            group.id
                        )))
                        .checked(!settings.off_in_new_sessions)
                        .disabled(!settings.enabled)
                        .with_size(Size::Small)
                        .on_click(move |on, _window, app| {
                            let off = !*on;
                            view.update(app, |this, cx| {
                                this.update_group(
                                    id_for_new_sessions,
                                    |s| s.off_in_new_sessions = off,
                                    cx,
                                );
                            });
                        }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child("On in new sessions"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "— unless the project remembers otherwise; switch it per \
                                 session in the input row",
                            ),
                    ),
            )
    }
}

impl Focusable for ToolsSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ToolsSection {
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .max_w(px(700.))
            .mx_auto()
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("BUILT-IN TOOLS"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "A group switched off here is hidden from every session. File, plan \
                         and skill tools are always on.",
                    ),
            )
            .child(
                div().flex().flex_col().gap_2().children(
                    GROUPS
                        .iter()
                        .map(|group| self.render_group(group, cx).into_any_element()),
                ),
            )
    }
}
