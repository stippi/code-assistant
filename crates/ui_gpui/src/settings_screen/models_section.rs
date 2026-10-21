//! Models settings section — per provider, the models it offers (from its
//! model listing) with a switch to show each in the model picker. Shown
//! models can be renamed and given overrides. Backed by
//! `model-selection.json`; the listings are cached in `model-discovery.json`.

use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, SharedString, Subscription, div, prelude::*, px,
};
use llm::capabilities::{Effort, ModelCapabilities};
use llm::discovery::DiscoveryCache;
use llm::model_selection::ModelSelection;
use llm::provider_config::{ConfigurationSystem, EditFormat, ModelConfig, ProvidersConfig};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

/// Listings older than this are refreshed when the section opens.
const LISTING_MAX_AGE_SECS: u64 = 24 * 60 * 60;
/// Select entry for "no override".
const MODEL_DEFAULT: &str = "Model default";

/// One model of a provider's list.
#[derive(Debug, Clone, PartialEq)]
struct ModelRow {
    key: String,
    id: String,
    name: String,
    /// Shown in the model picker.
    enabled: bool,
    capabilities: ModelCapabilities,
}

/// The rows of a provider: its listed models plus the shown ones it did not
/// list (added by hand, or gone from the listing), shown ones first, each
/// group by name; only those matching `filter` (name or ID).
fn provider_rows(config: &ConfigurationSystem, provider_id: &str, filter: &str) -> Vec<ModelRow> {
    let Some(provider) = config.providers.get(provider_id) else {
        return Vec::new();
    };
    let listed: Vec<String> = config
        .discovery
        .providers
        .get(provider_id)
        .map(|listing| listing.models.iter().map(|m| m.id.clone()).collect())
        .unwrap_or_default();
    let shown = config
        .models
        .values()
        .filter(|model| model.provider == provider_id)
        .map(|model| model.id.clone());
    let ids: HashSet<String> = listed.into_iter().chain(shown).collect();

    let filter = filter.trim().to_lowercase();
    let mut rows: Vec<ModelRow> = ids
        .into_iter()
        .map(|id| {
            let key = ModelConfig::key_for(provider_id, &id);
            let enabled = config.models.contains_key(&key);
            let capabilities = if enabled {
                config.model_capabilities(&key).unwrap_or_default()
            } else {
                let model = ModelConfig::new(provider_id, &id);
                ConfigurationSystem::api_client_kind_for_configs(&model, provider)
                    .map(|api| {
                        ModelCapabilities::resolve(
                            api,
                            &id,
                            config.discovery.model(provider_id, &id),
                        )
                    })
                    .unwrap_or_default()
            };
            let name = capabilities
                .display_name
                .clone()
                .unwrap_or_else(|| id.clone());
            ModelRow {
                key,
                id,
                name,
                enabled,
                capabilities,
            }
        })
        .filter(|row| {
            filter.is_empty()
                || row.name.to_lowercase().contains(&filter)
                || row.id.to_lowercase().contains(&filter)
        })
        .collect();
    rows.sort_by(|a, b| {
        b.enabled
            .cmp(&a.enabled)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    rows
}

/// "1M", "272K"
fn format_tokens(tokens: u32) -> String {
    if tokens >= 1_000_000 && tokens.is_multiple_of(1_000_000) {
        format!("{}M", tokens / 1_000_000)
    } else if tokens >= 1000 {
        format!("{}K", tokens / 1000)
    } else {
        tokens.to_string()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// The inputs of the override editor of the expanded model.
struct Editor {
    key: String,
    name: Entity<InputState>,
    behaves_like: Entity<InputState>,
    context_limit: Entity<InputState>,
    default_effort: Entity<SelectState<Vec<String>>>,
    edit_format: Entity<SelectState<Vec<String>>>,
    extra_config: Entity<TextareaState>,
    error: Option<String>,
}

pub struct ModelsSection {
    focus_handle: FocusHandle,
    providers: ProvidersConfig,
    selection: ModelSelection,
    discovery: DiscoveryCache,
    /// Providers whose model listing is being fetched.
    refreshing: HashSet<String>,
    filter_input: Entity<InputState>,
    /// "Add model ID" input per provider.
    add_inputs: HashMap<String, Entity<InputState>>,
    editor: Option<Editor>,
    _filter_subscription: Subscription,
}

impl ModelsSection {
    pub fn new(window: &mut gpui_kit::Window, cx: &mut Context<Self>) -> Self {
        let filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter models by name or ID"));
        let filter_subscription =
            cx.subscribe_in(&filter_input, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            });
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            providers: ProvidersConfig::new(),
            selection: ModelSelection::default(),
            discovery: DiscoveryCache::default(),
            refreshing: HashSet::new(),
            filter_input,
            add_inputs: HashMap::new(),
            editor: None,
            _filter_subscription: filter_subscription,
        };
        this.reload(window, cx);
        this
    }

    /// Reload providers, selection and listings from disk, and refresh
    /// listings that are missing or stale.
    pub fn reload(&mut self, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        self.providers = ConfigurationSystem::load_providers_config(None).unwrap_or_default();
        self.selection = ModelSelection::load_or_import().unwrap_or_else(|e| {
            warn!("Failed to load the model selection: {e:#}");
            ModelSelection::default()
        });
        self.discovery = DiscoveryCache::load();

        for provider_id in self.providers.keys() {
            if !self.add_inputs.contains_key(provider_id) {
                let input =
                    cx.new(|cx| InputState::new(window, cx).placeholder("Add a model ID by hand"));
                self.add_inputs.insert(provider_id.clone(), input);
            }
        }

        let stale: Vec<String> =
            self.providers
                .keys()
                .filter(|id| {
                    self.discovery.providers.get(*id).is_none_or(|listing| {
                        now_secs() - listing.fetched_at > LISTING_MAX_AGE_SECS
                    })
                })
                .cloned()
                .collect();
        for provider_id in stale {
            self.refresh(provider_id, cx);
        }
        cx.notify();
    }

    /// The configuration as the rest of the app sees it (models of
    /// providers that no longer exist are left out).
    fn view_config(&self) -> ConfigurationSystem {
        let selection = ModelSelection {
            models: self
                .selection
                .models
                .iter()
                .filter(|model| self.providers.contains_key(&model.provider))
                .cloned()
                .collect(),
        };
        ConfigurationSystem::from_parts(self.providers.clone(), selection, self.discovery.clone())
            .unwrap_or_default()
    }

    /// Fetch a provider's model listing on a background thread (with its
    /// own tokio runtime — gpui's executor is not tokio).
    fn refresh(&mut self, provider_id: String, cx: &mut Context<Self>) {
        let Some(provider) = self.providers.get(&provider_id).cloned() else {
            return;
        };
        if !self.refreshing.insert(provider_id.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(llm::discovery::list_models(&provider))
                })
                .await;
            this.update(cx, |this, cx| {
                this.refreshing.remove(&provider_id);
                // Another window may have refreshed other providers meanwhile.
                this.discovery = DiscoveryCache::load();
                this.discovery.record(&provider_id, result);
                if let Err(e) = this.discovery.save() {
                    warn!("Failed to save the model listings: {e:#}");
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = self.selection.save() {
            warn!("Failed to save the model selection: {e:#}");
        }
        cx.notify();
    }

    fn set_enabled(
        &mut self,
        provider_id: &str,
        model_id: &str,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        self.selection.set_enabled(provider_id, model_id, enabled);
        if !enabled
            && self.editor.as_ref().map(|e| e.key.as_str())
                == Some(ModelConfig::key_for(provider_id, model_id).as_str())
        {
            self.editor = None;
        }
        self.persist(cx);
    }

    fn add_by_hand(
        &mut self,
        provider_id: &str,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.add_inputs.get(provider_id).cloned() else {
            return;
        };
        let model_id = input.read(cx).value().trim().to_string();
        if model_id.is_empty() {
            return;
        }
        input.update(cx, |state, cx| state.set_value("", window, cx));
        self.set_enabled(provider_id, &model_id, true, cx);
    }

    fn toggle_editor(
        &mut self,
        row: &ModelRow,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        if self.editor.as_ref().is_some_and(|e| e.key == row.key) {
            self.editor = None;
            cx.notify();
            return;
        }
        let Some(model) = self.selection.get(&row.key).cloned() else {
            return;
        };
        let text_input = |placeholder: String,
                          value: Option<String>,
                          window: &mut gpui_kit::Window,
                          cx: &mut Context<Self>| {
            cx.new(|cx| {
                let state = InputState::new(window, cx).placeholder(placeholder);
                match value {
                    Some(value) => state.default_value(value),
                    None => state,
                }
            })
        };
        // Placeholders show what applies without an override.
        let defaults = {
            let mut without_overrides = model.clone();
            without_overrides.name = None;
            without_overrides.context_token_limit = None;
            let mut selection = self.selection.clone();
            if let Some(entry) = selection.get_mut(&row.key) {
                *entry = without_overrides;
            }
            ConfigurationSystem::from_parts(
                self.providers.clone(),
                selection,
                self.discovery.clone(),
            )
            .ok()
        };
        let default_name = defaults
            .as_ref()
            .map(|config| config.display_name(&row.key))
            .unwrap_or_else(|| row.id.clone());
        let default_context = defaults
            .as_ref()
            .and_then(|config| config.context_limit(&row.key))
            .map(|limit| limit.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let name = text_input(default_name, model.name.clone(), window, cx);
        let behaves_like = text_input(
            "catalog model ID, e.g. claude-opus-4-8".to_string(),
            model.behaves_like.clone(),
            window,
            cx,
        );
        let context_limit = text_input(
            default_context,
            model.context_token_limit.map(|limit| limit.to_string()),
            window,
            cx,
        );

        let mut effort_items = vec![MODEL_DEFAULT.to_string()];
        if let Some(reasoning) = &row.capabilities.reasoning {
            effort_items.extend(
                reasoning
                    .levels
                    .iter()
                    .map(|level| level.as_str().to_string()),
            );
        }
        let effort_value = model
            .default_effort
            .map(|effort| effort.as_str().to_string())
            .unwrap_or_else(|| MODEL_DEFAULT.to_string());
        let default_effort = cx.new(|cx| SelectState::new(effort_items, None, window, cx));
        default_effort.update(cx, |state, cx| {
            state.set_selected_value(&effort_value, window, cx)
        });

        let format_items = vec![
            MODEL_DEFAULT.to_string(),
            "simple".to_string(),
            "diff".to_string(),
        ];
        let format_value = match model.edit_format {
            Some(EditFormat::Simple) => "simple",
            Some(EditFormat::Diff) => "diff",
            None => MODEL_DEFAULT,
        }
        .to_string();
        let edit_format = cx.new(|cx| SelectState::new(format_items, None, window, cx));
        edit_format.update(cx, |state, cx| {
            state.set_selected_value(&format_value, window, cx)
        });

        let extra = if model.config.is_null() {
            String::new()
        } else {
            serde_json::to_string_pretty(&model.config).unwrap_or_default()
        };
        let extra_config = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 10)
                .placeholder("Extra request fields as JSON, merged into every request")
                .default_value(extra)
        });

        self.editor = Some(Editor {
            key: row.key.clone(),
            name,
            behaves_like,
            context_limit,
            default_effort,
            edit_format,
            extra_config,
            error: None,
        });
        cx.notify();
    }

    fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let text = |input: &Entity<InputState>, cx: &App| {
            Some(input.read(cx).value().trim().to_string()).filter(|s| !s.is_empty())
        };
        let selected = |select: &Entity<SelectState<Vec<String>>>, cx: &App| {
            select
                .read(cx)
                .selected_value()
                .cloned()
                .filter(|value| value != MODEL_DEFAULT)
        };

        let context_limit = match text(&editor.context_limit, cx) {
            None => None,
            Some(value) => match value.replace(['_', ','], "").parse::<u32>() {
                Ok(limit) => Some(limit),
                Err(_) => {
                    editor.error = Some(format!("Context window '{value}' is not a number"));
                    cx.notify();
                    return;
                }
            },
        };
        let extra = editor.extra_config.read(cx).value().trim().to_string();
        let config = if extra.is_empty() {
            serde_json::Value::Null
        } else {
            match serde_json::from_str::<serde_json::Value>(&extra) {
                Ok(value) if value.is_object() => value,
                _ => {
                    editor.error = Some("Extra request fields must be a JSON object".to_string());
                    cx.notify();
                    return;
                }
            }
        };

        let name = text(&editor.name, cx);
        let behaves_like = text(&editor.behaves_like, cx);
        let default_effort = selected(&editor.default_effort, cx).and_then(|v| Effort::parse(&v));
        let edit_format = selected(&editor.edit_format, cx).map(|value| match value.as_str() {
            "diff" => EditFormat::Diff,
            _ => EditFormat::Simple,
        });
        let key = editor.key.clone();
        let Some(model) = self.selection.get_mut(&key) else {
            return;
        };
        model.name = name;
        model.behaves_like = behaves_like;
        model.context_token_limit = context_limit;
        model.default_effort = default_effort;
        model.edit_format = edit_format;
        model.config = config;
        self.editor = None;
        self.persist(cx);
    }

    fn render_provider(
        &self,
        provider_id: &str,
        config: &ConfigurationSystem,
        filter: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let provider = &self.providers[provider_id];
        let rows = provider_rows(config, provider_id, filter);
        let listing = self.discovery.providers.get(provider_id);
        let shown = config
            .models
            .values()
            .filter(|model| model.provider == provider_id)
            .count();
        let refreshing = self.refreshing.contains(provider_id);
        let status = if refreshing {
            "Fetching models…".to_string()
        } else {
            match listing {
                None => "Not listed yet".to_string(),
                Some(listing) => {
                    let total = listing.models.len();
                    format!("{shown} of {total} listed models shown")
                }
            }
        };
        let error = listing.and_then(|listing| listing.error.clone());
        let id_for_refresh = provider_id.to_string();
        let id_for_add = provider_id.to_string();

        div()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .overflow_hidden()
            // Header
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(cx.theme().foreground)
                                    .child(SharedString::from(provider.label.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(status)),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("refresh-models-{provider_id}")))
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .when(!refreshing, |el| {
                                el.cursor_pointer()
                                    .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.refresh(id_for_refresh.clone(), cx);
                                    }))
                            })
                            .child(
                                Icon::default()
                                    .path(SharedString::from("icons/arrow_circle.svg"))
                                    .with_size(Size::XSmall),
                            )
                            .child("Refresh"),
                    ),
            )
            .when_some(error, |el, error| {
                el.child(
                    div()
                        .px_4()
                        .pb_2()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(SharedString::from(error)),
                )
            })
            // Model rows
            .child(
                div().flex().flex_col().children(
                    rows.into_iter()
                        .map(|row| self.render_row(provider_id, row, cx).into_any_element()),
                ),
            )
            // Add by hand
            .when_some(self.add_inputs.get(provider_id).cloned(), |el, input| {
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .py_2()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .flex_1()
                                .child(Input::new(&input).with_size(Size::Small)),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("add-model-{provider_id}")))
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .cursor_pointer()
                                .text_xs()
                                .bg(cx.theme().primary)
                                .text_color(cx.theme().primary_foreground)
                                .hover(|s| s.bg(cx.theme().primary.opacity(0.8)))
                                .child("Add")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.add_by_hand(&id_for_add, window, cx);
                                })),
                        ),
                )
            })
    }

    fn render_row(
        &self,
        provider_id: &str,
        row: ModelRow,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut badges = Vec::new();
        if let Some(context) = row.capabilities.context_window {
            badges.push(format!("{} context", format_tokens(context)));
        }
        if let Some(reasoning) = &row.capabilities.reasoning
            && let (Some(first), Some(last)) = (reasoning.levels.first(), reasoning.levels.last())
        {
            badges.push(if first == last {
                format!("effort {}", first.as_str())
            } else {
                format!("effort {}–{}", first.as_str(), last.as_str())
            });
        }
        if row.capabilities.edit_format == Some(EditFormat::Diff) {
            badges.push("diff edits".to_string());
        }
        let expanded = self.editor.as_ref().is_some_and(|e| e.key == row.key);
        let view = cx.entity();
        let provider_for_switch = provider_id.to_string();
        let id_for_switch = row.id.clone();
        let row_for_click = row.clone();

        div()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .child(
                        Switch::new(SharedString::from(format!("model-enabled-{}", row.key)))
                            .checked(row.enabled)
                            .with_size(Size::Small)
                            .on_click(move |enabled, _window, app| {
                                let enabled = *enabled;
                                let provider = provider_for_switch.clone();
                                let id = id_for_switch.clone();
                                view.update(app, |this, cx| {
                                    this.set_enabled(&provider, &id, enabled, cx);
                                });
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(if row.enabled {
                                        cx.theme().foreground
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .truncate()
                                    .child(SharedString::from(row.name.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground.opacity(0.8))
                                    .truncate()
                                    .child(SharedString::from(
                                        std::iter::once(row.id.clone())
                                            .chain(badges)
                                            .collect::<Vec<_>>()
                                            .join(" · "),
                                    )),
                            ),
                    )
                    .when(row.enabled, |el| {
                        el.child(
                            div()
                                .id(SharedString::from(format!("edit-model-{}", row.key)))
                                .p_1()
                                .rounded_md()
                                .cursor_pointer()
                                .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
                                .child(
                                    Icon::default()
                                        .path(SharedString::from(if expanded {
                                            "icons/chevron_up.svg"
                                        } else {
                                            "icons/chevron_down.svg"
                                        }))
                                        .with_size(Size::XSmall)
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.toggle_editor(&row_for_click, window, cx);
                                })),
                        )
                    }),
            )
            .when(expanded, |el| el.child(self.render_editor(cx)))
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(editor) = &self.editor else {
            return div().into_any_element();
        };
        let row = |label: &str, widget: gpui_kit::AnyElement, cx: &mut Context<Self>| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .w(px(110.))
                        .flex_none()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(label.to_string())),
                )
                .child(div().flex_1().min_w_0().child(widget))
        };
        let select = |state: &Entity<SelectState<Vec<String>>>| {
            Select::new(state).with_size(Size::Small).into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .py_3()
            .bg(cx.theme().background)
            .child(row(
                "Name",
                Input::new(&editor.name)
                    .with_size(Size::Small)
                    .into_any_element(),
                cx,
            ))
            .child(row(
                "Behaves like",
                Input::new(&editor.behaves_like)
                    .with_size(Size::Small)
                    .into_any_element(),
                cx,
            ))
            .child(row(
                "Context window",
                Input::new(&editor.context_limit)
                    .with_size(Size::Small)
                    .into_any_element(),
                cx,
            ))
            .child(row("Default effort", select(&editor.default_effort), cx))
            .child(row("Edit format", select(&editor.edit_format), cx))
            .child(row(
                "Extra fields",
                Textarea::new(&editor.extra_config).into_any_element(),
                cx,
            ))
            .when_some(editor.error.clone(), |el, error| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(SharedString::from(error)),
                )
            })
            .child(
                div().flex().justify_end().child(
                    div()
                        .id("save-model-overrides")
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .text_xs()
                        .bg(cx.theme().primary)
                        .text_color(cx.theme().primary_foreground)
                        .hover(|s| s.bg(cx.theme().primary.opacity(0.8)))
                        .child("Save")
                        .on_click(cx.listener(|this, _, _, cx| this.save_editor(cx))),
                ),
            )
            .into_any_element()
    }
}

impl Focusable for ModelsSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ModelsSection {
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let config = self.view_config();
        let filter = self.filter_input.read(cx).value().to_string();
        let mut provider_ids: Vec<String> = self.providers.keys().cloned().collect();
        provider_ids.sort_by_key(|id| self.providers[id].label.to_lowercase());

        div()
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .max_w(px(700.))
            .mx_auto()
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child("MODELS"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Choose which of your providers' models appear in the model picker. \
                         Context window, reasoning effort and request settings come from the \
                         built-in catalog and the provider; expand a shown model to override them.",
                    ),
            )
            .when(provider_ids.is_empty(), |el| {
                el.child(
                    div()
                        .py_8()
                        .flex()
                        .justify_center()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Add a provider in the Providers section first."),
                )
            })
            .when(!provider_ids.is_empty(), |el| {
                el.child(Input::new(&self.filter_input).with_size(Size::Small))
                    .children(provider_ids.iter().map(|provider_id| {
                        self.render_provider(provider_id, &config, &filter, cx)
                            .into_any_element()
                    }))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm::discovery::DiscoveredModel;
    use llm::provider_config::ProviderConfig;

    fn config(selection: Vec<ModelConfig>, listed: Vec<&str>) -> ConfigurationSystem {
        let providers = [(
            "anthropic".to_string(),
            ProviderConfig {
                label: "Anthropic".into(),
                provider: "anthropic".into(),
                config: serde_json::json!({}),
            },
        )]
        .into_iter()
        .collect();
        let mut discovery = DiscoveryCache::default();
        discovery.record(
            "anthropic",
            Ok(listed.into_iter().map(DiscoveredModel::new).collect()),
        );
        ConfigurationSystem::from_parts(providers, ModelSelection { models: selection }, discovery)
            .unwrap()
    }

    #[test]
    fn rows_list_shown_models_first_including_unlisted_ones() {
        let config = config(
            vec![
                ModelConfig::new("anthropic", "claude-opus-5"),
                ModelConfig::new("anthropic", "claude-private-preview"),
            ],
            vec!["claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5"],
        );
        let rows = provider_rows(&config, "anthropic", "");
        let names: Vec<(&str, bool)> = rows.iter().map(|r| (r.name.as_str(), r.enabled)).collect();
        assert_eq!(
            names,
            vec![
                ("Claude Opus 5", true),
                ("claude-private-preview", true),
                ("Claude Haiku 4.5", false),
                ("Claude Sonnet 5", false),
            ]
        );
        assert!(rows[0].capabilities.reasoning.is_some());
    }

    #[test]
    fn rows_filter_by_name_or_id() {
        let config = config(vec![], vec!["claude-opus-5", "claude-sonnet-5"]);
        let rows = provider_rows(&config, "anthropic", "SONNET");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "claude-sonnet-5");
        assert_eq!(
            provider_rows(&config, "anthropic", "opus-5")[0].id,
            "claude-opus-5"
        );
    }

    #[test]
    fn token_counts_are_short() {
        assert_eq!(format_tokens(1_000_000), "1M");
        assert_eq!(format_tokens(272_000), "272K");
        assert_eq!(format_tokens(1_048_576), "1048K");
    }
}
