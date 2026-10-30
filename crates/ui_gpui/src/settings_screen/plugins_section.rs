//! Plugins settings section — install a plugin from a git repository (or a
//! local folder), and switch, update or remove the installed ones. Backed by
//! `<config_dir>/plugins/` (`code_assistant_core::plugin_host::PluginStore`).
//!
//! Installing and updating run git, so they run off the main thread; the
//! list shows what each plugin contributes. Changes apply at once: provider
//! templates show in the next *Add provider*, MCP servers join the next agent
//! run, file renderers the Files view.

use code_assistant_core::plugin_host::{PluginListing, PluginSource, PluginStore};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable, Size};
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, SharedString, Task, div, prelude::*, px,
};
use std::collections::HashSet;

/// What runs in the background.
#[derive(Clone, PartialEq)]
enum Busy {
    Installing,
    Updating(String),
}

pub struct PluginsSection {
    focus_handle: FocusHandle,
    plugins: Vec<PluginListing>,
    source_input: Entity<InputState>,
    busy: Option<Busy>,
    /// The outcome of the last install, update or removal.
    message: Option<Result<String, String>>,
    /// Plugins whose removal waits for a second click.
    confirm_remove: HashSet<String>,
    _task: Option<Task<()>>,
}

impl PluginsSection {
    pub fn new(window: &mut gpui_kit::Window, cx: &mut Context<Self>) -> Self {
        let source_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Git URL, owner/repo, …#branch-or-tag, or a local folder")
        });
        Self {
            focus_handle: cx.focus_handle(),
            plugins: PluginStore::new().list(),
            source_input,
            busy: None,
            message: None,
            confirm_remove: HashSet::new(),
            _task: None,
        }
    }

    /// Reload the installed plugins from disk.
    pub fn reload(&mut self) {
        self.plugins = PluginStore::new().list();
        self.confirm_remove.clear();
    }

    fn install(&mut self, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        let source = self.source_input.read(cx).value().trim().to_string();
        if source.is_empty() || self.busy.is_some() {
            return;
        }
        self.busy = Some(Busy::Installing);
        self.message = None;
        cx.notify();
        let job = cx.background_spawn(async move { PluginStore::new().install(&source) });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                this.message = Some(match result {
                    Ok(listing) => {
                        this.source_input.update(cx, |input, cx| {
                            input.set_value(SharedString::default(), window, cx)
                        });
                        Ok(format!("Installed '{}'.", listing.name))
                    }
                    Err(e) => Err(format!("{e:#}")),
                });
                this.reload();
                crate::shared::custom_renderers::reload(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn update(&mut self, name: String, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(Busy::Updating(name.clone()));
        self.message = None;
        cx.notify();
        let job = cx.background_spawn({
            let name = name.clone();
            async move { PluginStore::new().update(&name) }
        });
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.busy = None;
                this.message = Some(match result {
                    Ok(_) => Ok(format!("Updated '{name}'.")),
                    Err(e) => Err(format!("Could not update '{name}': {e:#}")),
                });
                this.reload();
                crate::shared::custom_renderers::reload(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn remove(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.confirm_remove.remove(name) {
            self.confirm_remove.insert(name.to_string());
            cx.notify();
            return;
        }
        self.message = Some(match PluginStore::new().remove(name) {
            Ok(()) => Ok(format!("Removed '{name}'.")),
            Err(e) => Err(format!("Could not remove '{name}': {e:#}")),
        });
        self.reload();
        crate::shared::custom_renderers::reload(cx);
        cx.notify();
    }

    fn set_enabled(&mut self, name: &str, enabled: bool, cx: &mut Context<Self>) {
        if let Err(e) = PluginStore::new().set_enabled(name, enabled) {
            self.message = Some(Err(format!("{e:#}")));
        }
        self.reload();
        crate::shared::custom_renderers::reload(cx);
        cx.notify();
    }

    fn render_install_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let installing = self.busy == Some(Busy::Installing);
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.source_input)),
            )
            .child(
                div()
                    .id("plugin-install-btn")
                    .flex_none()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .bg(cx.theme().primary)
                    .text_color(cx.theme().primary_foreground)
                    .when(self.busy.is_none(), |el| {
                        el.cursor_pointer()
                            .hover(|s| s.opacity(0.9))
                            .on_click(cx.listener(|this, _, window, cx| this.install(window, cx)))
                    })
                    .when(self.busy.is_some(), |el| el.opacity(0.6))
                    .child(if installing {
                        "Installing…"
                    } else {
                        "Install"
                    }),
            )
    }

    fn render_action(
        &self,
        id: String,
        label: &'static str,
        danger: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        let color = if danger {
            cx.theme().danger
        } else {
            cx.theme().muted_foreground
        };
        div()
            .id(SharedString::from(id))
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .text_xs()
            .text_color(color)
            .hover(|s| s.bg(cx.theme().muted))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
    }

    fn render_plugin_card(
        &self,
        plugin: &PluginListing,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name = plugin.name.clone();
        let view = cx.entity();
        let title = match &plugin.manifest {
            Ok(manifest) => match &manifest.version {
                Some(version) => format!("{name} {version}"),
                None => name.clone(),
            },
            Err(_) => name.clone(),
        };
        let mut source = plugin.installed.source.display();
        if let Some(commit) = &plugin.installed.commit {
            source.push_str(&format!(" · {}", &commit[..commit.len().min(10)]));
        }
        if matches!(plugin.installed.source, PluginSource::Local { .. }) {
            source.push_str(" · linked folder");
        }
        let updating = self.busy == Some(Busy::Updating(name.clone()));
        let confirming = self.confirm_remove.contains(&name);
        div()
            .flex()
            .flex_col()
            .gap_1()
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
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(cx.theme().foreground)
                            .child(SharedString::from(title)),
                    )
                    .child(self.render_action(
                        format!("plugin-update-{name}"),
                        if updating { "Updating…" } else { "Update" },
                        false,
                        cx,
                        {
                            let name = name.clone();
                            move |this, cx| this.update(name.clone(), cx)
                        },
                    ))
                    .child(self.render_action(
                        format!("plugin-remove-{name}"),
                        if confirming {
                            "Really remove?"
                        } else {
                            "Remove"
                        },
                        true,
                        cx,
                        {
                            let name = name.clone();
                            move |this, cx| this.remove(&name, cx)
                        },
                    ))
                    .child(
                        Switch::new(SharedString::from(format!("plugin-enabled-{name}")))
                            .checked(plugin.installed.enabled)
                            .with_size(Size::Small)
                            .on_click({
                                let name = name.clone();
                                move |on, _window, app| {
                                    let on = *on;
                                    view.update(app, |this, cx| this.set_enabled(&name, on, cx));
                                }
                            }),
                    ),
            )
            .child(match &plugin.manifest {
                Ok(manifest) => div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when_some(manifest.description.clone(), |el, description| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().foreground)
                                .child(SharedString::from(description)),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(manifest.contributions_summary())),
                    )
                    .into_any_element(),
                Err(error) => div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(SharedString::from(format!("Not loaded: {error}")))
                    .into_any_element(),
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(SharedString::from(source)),
            )
    }
}

impl Focusable for PluginsSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PluginsSection {
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let plugins = self.plugins.clone();
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
                    .child("PLUGINS"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Plugins add provider templates, MCP servers and file renderers. Install \
                         one from its git repository; it is cloned with your git setup, so \
                         private repositories work too. Plugins can start programs on this \
                         computer — install only plugins you trust.",
                    ),
            )
            .child(self.render_install_row(cx))
            .when_some(self.message.clone(), |el, message| {
                let (text, color) = match message {
                    Ok(text) => (text, cx.theme().muted_foreground),
                    Err(text) => (text, cx.theme().danger),
                };
                el.child(
                    div()
                        .text_xs()
                        .text_color(color)
                        .whitespace_normal()
                        .child(SharedString::from(text)),
                )
            })
            .when(plugins.is_empty(), |el| {
                el.child(
                    div()
                        .py_8()
                        .flex()
                        .justify_center()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No plugins installed"),
                )
            })
            .child(
                div().flex().flex_col().gap_2().children(
                    plugins
                        .iter()
                        .map(|plugin| self.render_plugin_card(plugin, cx).into_any_element()),
                ),
            )
    }
}
