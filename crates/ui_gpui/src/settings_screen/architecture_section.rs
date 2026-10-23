//! Architecture overview settings: the model explaining components, and
//! when the overview updates. Stored in `architecture.json`.

use code_assistant_core::architecture::ArchitectureConfig;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, SharedString, Subscription, div, prelude::*, px,
};
use tracing::warn;

/// The choice standing for "the default model of new sessions".
const DEFAULT_CHOICE: &str = "Default model";

pub struct ArchitectureSection {
    focus_handle: FocusHandle,
    config: ArchitectureConfig,
    model_select: Entity<SelectState<Vec<String>>>,
    _model_subscription: Subscription,
}

impl ArchitectureSection {
    pub fn new(window: &mut gpui_kit::Window, cx: &mut Context<Self>) -> Self {
        let model_select = cx.new(|cx| SelectState::new(Vec::<String>::new(), None, window, cx));
        let model_subscription = cx.subscribe_in(&model_select, window, Self::on_model_selected);
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            config: ArchitectureConfig::load(),
            model_select,
            _model_subscription: model_subscription,
        };
        this.reload(window, cx);
        this
    }

    /// Re-read the models (the Models section may have changed them) and
    /// the stored settings.
    pub fn reload(&mut self, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        self.config = ArchitectureConfig::load();
        let mut models = llm::provider_config::ConfigurationSystem::load()
            .map(|config| config.list_models())
            .unwrap_or_default();
        models.sort();
        let selected = if models.contains(&self.config.model) {
            self.config.model.clone()
        } else {
            DEFAULT_CHOICE.to_owned()
        };
        let items: Vec<String> = std::iter::once(DEFAULT_CHOICE.to_owned())
            .chain(models)
            .collect();
        self.model_select.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            state.set_selected_value(&selected, window, cx);
        });
    }

    fn on_model_selected(
        &mut self,
        _: &Entity<SelectState<Vec<String>>>,
        event: &SelectEvent<Vec<String>>,
        _window: &mut gpui_kit::Window,
        _cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(model)) = event else {
            return;
        };
        self.config.model = if model == DEFAULT_CHOICE {
            String::new()
        } else {
            model.clone()
        };
        self.save();
    }

    fn update(&mut self, change: impl FnOnce(&mut ArchitectureConfig), cx: &mut Context<Self>) {
        change(&mut self.config);
        self.save();
        cx.notify();
    }

    fn save(&self) {
        if let Err(e) = self.config.save() {
            warn!("Failed to save the architecture settings: {e:#}");
        }
    }

    fn render_switch(
        &self,
        id: &'static str,
        checked: bool,
        label: &'static str,
        hint: &'static str,
        set: fn(&mut ArchitectureConfig, bool),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Switch::new(id)
                            .checked(checked)
                            .with_size(Size::Small)
                            .on_click(move |enabled, _window, app| {
                                let enabled = *enabled;
                                view.update(app, |this, cx| {
                                    this.update(|config| set(config, enabled), cx)
                                });
                            }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(label),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(hint),
            )
    }
}

fn caption(text: &str, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(cx.theme().muted_foreground)
        .child(SharedString::from(text.to_string()))
}

impl Focusable for ArchitectureSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ArchitectureSection {
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let auto_summarize = self.config.auto_summarize;
        let watch = self.config.watch;
        div()
            .flex()
            .flex_col()
            .gap_5()
            .w_full()
            .max_w(px(700.))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("ARCHITECTURE"),
            )
            .child(
                div()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(
                        "The Architecture panel shows a project as boxes of components with \
                         arrows to what each uses. The structure and dependencies are read \
                         from the code without a model; the model only explains what each \
                         component is for, its constraints and key interfaces — one request \
                         per component, repeated only when its public shape changes.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(caption("Model", cx))
                    .child(
                        div().w(px(280.)).child(
                            Select::new(&self.model_select)
                                .placeholder("Select model")
                                .with_size(Size::Small)
                                .icon(
                                    Icon::default()
                                        .path("icons/chevron_up_down.svg")
                                        .with_size(Size::XSmall)
                                        .text_color(cx.theme().muted_foreground),
                                ),
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "A fast, inexpensive model is enough: it reads short digests \
                                 (names, docs, public signatures), never whole files.",
                            ),
                    ),
            )
            .child(self.render_switch(
                "architecture-auto-summarize",
                auto_summarize,
                "Explain components automatically",
                "Ask the model when a level is opened or its component changed. Off, only \
                 the Explain button asks.",
                |config, enabled| config.auto_summarize = enabled,
                cx,
            ))
            .child(self.render_switch(
                "architecture-watch",
                watch,
                "Follow file changes",
                "Rescan while the panel is open when source files or manifests change, \
                 including uncommitted edits.",
                |config, enabled| config.watch = enabled,
                cx,
            ))
    }
}
