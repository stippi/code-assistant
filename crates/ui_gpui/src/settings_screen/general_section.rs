//! General settings section — theme, scale, and other global preferences.

use crate::input::model_selector::ModelItem;
use code_assistant_core::session::auto_mode::AutoModeConfig;
use code_assistant_core::session::default_model::DefaultModelConfig;
use code_assistant_core::session::idle_handoff::HandoffConfig;
use code_assistant_core::session::lifecycle::LifecycleConfig;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, SharedString, Subscription, div, prelude::*, px,
};
use tracing::warn;

pub struct GeneralSection {
    focus_handle: FocusHandle,
    /// The model new sessions start with; stored in `ui-settings.json`.
    default_model_select: Entity<SelectState<Vec<ModelItem>>>,
    _default_model_subscription: Subscription,
    /// Input tokens of a session's last request from which an idle session
    /// gets a prepared handoff; stored in `handoff.json`.
    handoff_threshold_input: Entity<InputState>,
    _handoff_threshold_subscription: Subscription,
    /// Days of inactivity after which a session settles; stored in
    /// `lifecycle.json` together with the merge rule.
    settle_days_input: Entity<InputState>,
    _settle_days_subscription: Subscription,
    settle_on_merge: bool,
    /// The model reviewing tool calls in auto mode; stored in
    /// `auto-mode.json` together with the extra rules.
    auto_mode_model_select: Entity<SelectState<Vec<ModelItem>>>,
    _auto_mode_model_subscription: Subscription,
    auto_mode_rules_input: Entity<TextareaState>,
    _auto_mode_rules_subscription: Subscription,
}

impl GeneralSection {
    pub fn new(window: &mut gpui_kit::Window, cx: &mut Context<Self>) -> Self {
        let default_model_select =
            cx.new(|cx| SelectState::new(Vec::<ModelItem>::new(), None, window, cx));
        let default_model_subscription = cx.subscribe_in(
            &default_model_select,
            window,
            Self::on_default_model_selected,
        );
        let threshold = HandoffConfig::load().idle_threshold_tokens;
        let handoff_threshold_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("150000")
                .default_value(threshold.to_string())
        });
        let subscription = cx.subscribe_in(
            &handoff_threshold_input,
            window,
            Self::on_handoff_threshold_input,
        );
        let lifecycle = LifecycleConfig::load();
        let settle_days_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("14")
                .default_value(lifecycle.auto_settle_after_days.to_string())
        });
        let settle_days_subscription =
            cx.subscribe_in(&settle_days_input, window, Self::on_settle_days_input);
        let auto_mode_model_select =
            cx.new(|cx| SelectState::new(Vec::<ModelItem>::new(), None, window, cx));
        let auto_mode_model_subscription = cx.subscribe_in(
            &auto_mode_model_select,
            window,
            Self::on_auto_mode_model_selected,
        );
        let auto_mode_rules = AutoModeConfig::load().instructions;
        let auto_mode_rules_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder("e.g. Pushing to my own feature branches is fine.")
                .default_value(auto_mode_rules)
        });
        let auto_mode_rules_subscription = cx.subscribe_in(
            &auto_mode_rules_input,
            window,
            Self::on_auto_mode_rules_input,
        );
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            default_model_select,
            _default_model_subscription: default_model_subscription,
            handoff_threshold_input,
            _handoff_threshold_subscription: subscription,
            settle_days_input,
            _settle_days_subscription: settle_days_subscription,
            settle_on_merge: lifecycle.auto_settle_on_merge,
            auto_mode_model_select,
            _auto_mode_model_subscription: auto_mode_model_subscription,
            auto_mode_rules_input,
            _auto_mode_rules_subscription: auto_mode_rules_subscription,
        };
        this.reload(window, cx);
        this
    }

    /// Re-read the offered models and the stored default, e.g. after the
    /// Models section changed the selection.
    pub fn reload(&mut self, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        let config = llm::provider_config::ConfigurationSystem::load().ok();
        let models = config
            .as_ref()
            .map(|config| ModelItem::for_models(config, None))
            .unwrap_or_default();
        let default_model = config.as_ref().and_then(|config| {
            DefaultModelConfig::load()
                .model
                .and_then(|model| config.resolve_key(&model))
        });
        let auto_mode_model = config.as_ref().and_then(|config| {
            AutoModeConfig::load()
                .model
                .and_then(|model| config.resolve_key(&model))
        });
        let mut auto_mode_models = vec![ModelItem::placeholder("Same as the session")];
        auto_mode_models.extend(models.iter().cloned());
        self.default_model_select.update(cx, |state, cx| {
            state.set_items(models, window, cx);
            if let Some(model) = &default_model {
                state.set_selected_value(model, window, cx);
            }
        });
        self.auto_mode_model_select.update(cx, |state, cx| {
            state.set_items(auto_mode_models, window, cx);
            state.set_selected_value(&auto_mode_model.unwrap_or_default(), window, cx);
        });
    }

    fn on_auto_mode_model_selected(
        &mut self,
        _: &Entity<SelectState<Vec<ModelItem>>>,
        event: &SelectEvent<Vec<ModelItem>>,
        _window: &mut gpui_kit::Window,
        _cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(model)) = event else {
            return;
        };
        let model = (!model.is_empty()).then(|| model.clone());
        Self::save_auto_mode_config(|config| config.model = model);
    }

    fn on_auto_mode_rules_input(
        &mut self,
        input: &Entity<TextareaState>,
        event: &InputEvent,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let rules = input.read(cx).value().to_string();
        Self::save_auto_mode_config(|config| config.instructions = rules);
    }

    /// Change one setting on top of the stored config, keeping the others
    /// (e.g. an effort only set in the file).
    fn save_auto_mode_config(change: impl FnOnce(&mut AutoModeConfig)) {
        let mut config = AutoModeConfig::load();
        change(&mut config);
        if let Err(e) = config.save() {
            warn!("Failed to save the auto mode settings: {e:#}");
        }
    }

    fn on_default_model_selected(
        &mut self,
        _: &Entity<SelectState<Vec<ModelItem>>>,
        event: &SelectEvent<Vec<ModelItem>>,
        _window: &mut gpui_kit::Window,
        _cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(model)) = event else {
            return;
        };
        // Every running frontend starts its next new session with it.
        let config = DefaultModelConfig {
            model: Some(model.clone()),
        };
        if let Err(e) = config.save() {
            warn!("Failed to save the default model: {e:#}");
        }
    }

    fn on_settle_days_input(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let Ok(days) = input.read(cx).value().trim().parse::<u32>() else {
            return;
        };
        self.save_lifecycle_config(|config| config.auto_settle_after_days = days, cx);
    }

    fn set_settle_on_merge(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settle_on_merge = enabled;
        self.save_lifecycle_config(|config| config.auto_settle_on_merge = enabled, cx);
        cx.notify();
    }

    /// Change one rule on top of the stored config, so the other stays as
    /// another instance may have left it.
    fn save_lifecycle_config(
        &self,
        change: impl FnOnce(&mut LifecycleConfig),
        _cx: &mut Context<Self>,
    ) {
        let mut config = LifecycleConfig::load();
        change(&mut config);
        if let Err(e) = config.save() {
            warn!("Failed to save the lifecycle settings: {e:#}");
        }
    }

    /// Save the threshold whenever the input holds a valid number.
    fn on_handoff_threshold_input(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let Ok(idle_threshold_tokens) = input.read(cx).value().trim().parse::<u64>() else {
            return;
        };
        let config = HandoffConfig {
            idle_threshold_tokens,
        };
        if let Err(e) = config.save() {
            warn!("Failed to save the handoff settings: {e:#}");
        }
    }
}

impl Focusable for GeneralSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GeneralSection {
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
            // Header
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("GENERAL"),
            )
            // Info
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child("Theme and zoom controls are available in the titlebar."),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "Configuration files are stored in {}",
                                code_assistant_core::config_dir::config_dir().display()
                            )),
                    ),
            )
            // Config paths
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(cx.theme().muted_foreground)
                            .child("Configuration Files"),
                    )
                    .child(Self::render_config_path(
                        "Providers",
                        &llm::provider_config::ConfigurationSystem::providers_config_path()
                            .display()
                            .to_string(),
                        cx,
                    ))
                    .child(Self::render_config_path(
                        "Models",
                        &llm::model_selection::ModelSelection::path()
                            .display()
                            .to_string(),
                        cx,
                    )),
            )
            // Default model
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(cx.theme().muted_foreground)
                            .child("Default model"),
                    )
                    .child(
                        div().w(px(280.)).child(
                            Select::new(&self.default_model_select)
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
                                "New sessions start with this model, also in the terminal \
                                 and over ACP, unless --model is given. Existing sessions \
                                 keep their model.",
                            ),
                    ),
            )
            // Auto mode
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(cx.theme().muted_foreground)
                            .child("Auto mode"),
                    )
                    .child(
                        div().w(px(280.)).child(
                            Select::new(&self.auto_mode_model_select)
                                .placeholder("Same as the session")
                                .with_size(Size::Small)
                                .icon(
                                    Icon::default()
                                        .path("icons/chevron_up_down.svg")
                                        .with_size(Size::XSmall)
                                        .text_color(cx.theme().muted_foreground),
                                ),
                        ),
                    )
                    .child(Textarea::new(&self.auto_mode_rules_input))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "With the Auto Mode permission setting, this model reviews \
                                 every tool call that may change something against what you \
                                 asked for, in all projects; you are only asked about the calls \
                                 it holds back. A small, fast model keeps the agent quick. The \
                                 rules above are added to its instructions.",
                            ),
                    ),
            )
            // Prepared handoff
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(cx.theme().muted_foreground)
                            .child("Handoff when idle"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(160.))
                                    .child(Input::new(&self.handoff_threshold_input)),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().foreground)
                                    .child("input tokens"),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "When a session whose last request had at least this many \
                                 input tokens stays idle for four minutes, the agent prepares \
                                 a /new prompt for a follow-up session while the prompt cache \
                                 is still warm. 0 turns this off.",
                            ),
                    ),
            )
            // Settled sessions
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(cx.theme().muted_foreground)
                            .child("Settled sessions"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().w(px(80.)).child(Input::new(&self.settle_days_input)))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().foreground)
                                    .child("days without activity"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Switch::new("settle-on-merge")
                                    .checked(self.settle_on_merge)
                                    .with_size(Size::Small)
                                    .on_click({
                                        let view = cx.entity();
                                        move |enabled, _window, app| {
                                            let enabled = *enabled;
                                            view.update(app, |this, cx| {
                                                this.set_settle_on_merge(enabled, cx);
                                            });
                                        }
                                    }),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().foreground)
                                    .child("Settle a session once its branch is merged"),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "A session that saw no activity for this many days leaves \
                                 the inbox for the Settled shelf; 0 keeps every session in \
                                 the inbox. The merge rule applies to sessions that work on \
                                 a branch of their own and also recognises squash merges. \
                                 Un-settling a session keeps it in the inbox until it is \
                                 active again.",
                            ),
                    ),
            )
    }
}

impl GeneralSection {
    fn render_config_path(label: &str, path: &str, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .min_w(px(80.))
                    .child(SharedString::from(format!("{}:", label))),
            )
            .child(
                div()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(cx.theme().foreground)
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .bg(cx.theme().muted)
                    .child(SharedString::from(path.to_string())),
            )
    }
}
