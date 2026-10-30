//! Plugins settings section — install a plugin from a git repository (or a
//! local folder), and switch, update or remove the installed ones. A
//! repository holding a marketplace is added as one instead; its plugins are
//! listed with a switch that installs and enables them. Backed by
//! `<config_dir>/plugins/` (`code_assistant_core::plugin_host::PluginStore`).
//!
//! Installing and updating run git, so they run off the main thread; the
//! list shows what each plugin contributes. Changes apply at once: provider
//! templates show in the next *Add provider*, MCP servers join the next agent
//! run, file renderers the Files view.

use code_assistant_core::plugin_host::{
    Added, MarketplaceListing, PluginListing, PluginSource, PluginStore,
};
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
    UpdatingMarketplace(String),
    /// Installing `<plugin>@<marketplace>` from its switch.
    InstallingFrom(String),
}

pub struct PluginsSection {
    focus_handle: FocusHandle,
    plugins: Vec<PluginListing>,
    marketplaces: Vec<MarketplaceListing>,
    source_input: Entity<InputState>,
    busy: Option<Busy>,
    /// The outcome of the last install, update or removal.
    message: Option<Result<String, String>>,
    /// Plugins whose removal waits for a second click.
    confirm_remove: HashSet<String>,
    /// Marketplaces whose removal waits for a second click.
    confirm_remove_marketplace: HashSet<String>,
    _task: Option<Task<()>>,
}

impl PluginsSection {
    pub fn new(window: &mut gpui_kit::Window, cx: &mut Context<Self>) -> Self {
        let source_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Plugin or marketplace: git URL, owner/repo, …#branch, or a folder")
        });
        let store = PluginStore::new();
        Self {
            focus_handle: cx.focus_handle(),
            plugins: store.list(),
            marketplaces: store.list_marketplaces(),
            source_input,
            busy: None,
            message: None,
            confirm_remove: HashSet::new(),
            confirm_remove_marketplace: HashSet::new(),
            _task: None,
        }
    }

    /// Reload the installed plugins and added marketplaces from disk.
    pub fn reload(&mut self) {
        let store = PluginStore::new();
        self.plugins = store.list();
        self.marketplaces = store.list_marketplaces();
        self.confirm_remove.clear();
        self.confirm_remove_marketplace.clear();
    }

    /// Run `job` (git work) off the main thread; its outcome becomes the
    /// message.
    fn run_job(
        &mut self,
        busy: Busy,
        job: impl FnOnce() -> anyhow::Result<String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(busy);
        self.message = None;
        cx.notify();
        let job = cx.background_spawn(async move { job() });
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.busy = None;
                this.message = Some(result.map_err(|e| format!("{e:#}")));
                this.reload();
                crate::shared::custom_renderers::reload(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn install(&mut self, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        let source = self.source_input.read(cx).value().trim().to_string();
        if source.is_empty() || self.busy.is_some() {
            return;
        }
        self.busy = Some(Busy::Installing);
        self.message = None;
        cx.notify();
        let job = cx.background_spawn(async move { PluginStore::new().add(&source) });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                this.message = Some(match result {
                    Ok(added) => {
                        this.source_input.update(cx, |input, cx| {
                            input.set_value(SharedString::default(), window, cx)
                        });
                        Ok(match added {
                            Added::Plugin(listing) => format!("Installed '{}'.", listing.name),
                            Added::Marketplace(listing) => format!(
                                "Added marketplace '{}': switch on the plugins you want below.",
                                listing.name
                            ),
                        })
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
        self.run_job(
            Busy::Updating(name.clone()),
            move || {
                PluginStore::new()
                    .update(&name)
                    .map(|_| format!("Updated '{name}'."))
                    .map_err(|e| e.context(format!("Could not update '{name}'")))
            },
            cx,
        );
    }

    fn update_marketplace(&mut self, name: String, cx: &mut Context<Self>) {
        self.run_job(
            Busy::UpdatingMarketplace(name.clone()),
            move || {
                PluginStore::new()
                    .update_marketplace(&name)
                    .map(|_| format!("Updated marketplace '{name}' and its plugins."))
                    .map_err(|e| e.context(format!("Could not update marketplace '{name}'")))
            },
            cx,
        );
    }

    fn remove_marketplace(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.confirm_remove_marketplace.remove(name) {
            self.confirm_remove_marketplace.insert(name.to_string());
            cx.notify();
            return;
        }
        self.message = Some(match PluginStore::new().remove_marketplace(name) {
            Ok(()) => Ok(format!(
                "Removed marketplace '{name}' and the plugins installed from it."
            )),
            Err(e) => Err(format!("Could not remove marketplace '{name}': {e:#}")),
        });
        self.reload();
        crate::shared::custom_renderers::reload(cx);
        cx.notify();
    }

    /// A marketplace plugin's switch: on installs it (once) and enables it,
    /// off disables it and keeps it installed.
    fn switch_marketplace_plugin(
        &mut self,
        plugin: String,
        marketplace: String,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        if self.plugins.iter().any(|p| p.name == plugin) {
            self.set_enabled(&plugin, on, cx);
            return;
        }
        if !on {
            return;
        }
        let source = format!("{plugin}@{marketplace}");
        self.run_job(
            Busy::InstallingFrom(source.clone()),
            move || {
                PluginStore::new()
                    .install(&source)
                    .map(|listing| format!("Installed '{}'.", listing.name))
            },
            cx,
        );
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

    fn render_marketplace_card(
        &self,
        marketplace: &MarketplaceListing,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name = marketplace.name.clone();
        let view = cx.entity();
        let mut source = marketplace.installed.source.display();
        if let Some(commit) = &marketplace.installed.commit {
            source.push_str(&format!(" · {}", &commit[..commit.len().min(10)]));
        }
        if matches!(marketplace.installed.source, PluginSource::Local { .. }) {
            source.push_str(" · linked folder");
        }
        let updating = self.busy == Some(Busy::UpdatingMarketplace(name.clone()));
        let confirming = self.confirm_remove_marketplace.contains(&name);
        let header = div()
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
                    .child(SharedString::from(name.clone())),
            )
            .child(self.render_action(
                format!("marketplace-update-{name}"),
                if updating { "Updating…" } else { "Update" },
                false,
                cx,
                {
                    let name = name.clone();
                    move |this, cx| this.update_marketplace(name.clone(), cx)
                },
            ))
            .child(self.render_action(
                format!("marketplace-remove-{name}"),
                if confirming {
                    "Remove with its plugins?"
                } else {
                    "Remove"
                },
                true,
                cx,
                {
                    let name = name.clone();
                    move |this, cx| this.remove_marketplace(&name, cx)
                },
            ));
        let body = match &marketplace.manifest {
            Ok(manifest) => div()
                .flex()
                .flex_col()
                .gap_2()
                .when_some(manifest.description.clone(), |el, description| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().foreground)
                            .child(SharedString::from(description)),
                    )
                })
                .children(manifest.plugins.iter().map(|entry| {
                    let installed = self.plugins.iter().find(|p| p.name == entry.name);
                    let from_here = installed.is_none_or(|p| {
                        p.installed.source
                            == PluginSource::Marketplace {
                                marketplace: name.clone(),
                            }
                    });
                    let installing =
                        self.busy == Some(Busy::InstallingFrom(format!("{}@{name}", entry.name)));
                    let title = match &entry.version {
                        Some(version) => format!("{} {version}", entry.name),
                        None => entry.name.clone(),
                    };
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .text_color(cx.theme().foreground)
                                        .child(SharedString::from(title)),
                                )
                                .when_some(entry.description.clone(), |el, description| {
                                    el.child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(SharedString::from(description)),
                                    )
                                }),
                        )
                        .when(installing, |el| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Installing…"),
                            )
                        })
                        .child(if from_here {
                            let plugin = entry.name.clone();
                            let marketplace = name.clone();
                            let view = view.clone();
                            Switch::new(SharedString::from(format!(
                                "marketplace-plugin-{name}-{}",
                                entry.name
                            )))
                            .checked(installed.is_some_and(|p| p.installed.enabled))
                            .with_size(Size::Small)
                            .on_click(move |on, _window, app| {
                                let on = *on;
                                view.update(app, |this, cx| {
                                    this.switch_marketplace_plugin(
                                        plugin.clone(),
                                        marketplace.clone(),
                                        on,
                                        cx,
                                    )
                                });
                            })
                            .into_any_element()
                        } else {
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("Installed from elsewhere")
                                .into_any_element()
                        })
                })),
            Err(error) => div()
                .text_xs()
                .text_color(cx.theme().danger)
                .child(SharedString::from(format!("Not loaded: {error}"))),
        };
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
            .child(header)
            .child(body)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(SharedString::from(source)),
            )
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
        let marketplaces = self.marketplaces.clone();
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
                         one from its git repository, or add a marketplace — a repository \
                         listing several plugins — and switch on the ones you want. Repositories \
                         are cloned with your git setup (credential helpers, SSH agent), so \
                         private ones work too. Plugins can start programs on this computer — \
                         install only plugins you trust.",
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
            .when(!marketplaces.is_empty(), |el| {
                el.child(
                    div()
                        .pt_2()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .child("MARKETPLACES"),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(marketplaces.iter().map(|marketplace| {
                            self.render_marketplace_card(marketplace, cx)
                                .into_any_element()
                        })),
                )
                .child(
                    div()
                        .pt_2()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .child("INSTALLED"),
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
