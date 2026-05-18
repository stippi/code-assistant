//! Models settings section — list configured models, add/remove.

use gpui::{div, prelude::*, px, App, Context, FocusHandle, Focusable, SharedString};
use gpui_component::ActiveTheme;
use std::collections::BTreeMap;
use tracing::warn;

/// A single model entry from models.json.
#[derive(Clone, Debug)]
pub struct ModelEntry {
    pub name: String,
    pub provider: String,
    pub model_id: String,
    pub context_limit: u32,
}

pub struct ModelsSection {
    focus_handle: FocusHandle,
    models: Vec<ModelEntry>,
}

impl ModelsSection {
    pub fn new(_window: &mut gpui::Window, cx: &mut Context<Self>) -> Self {
        let models = Self::load_models();
        Self {
            focus_handle: cx.focus_handle(),
            models,
        }
    }

    /// Reload models from disk (called when this section becomes visible).
    pub fn reload(&mut self) {
        self.models = Self::load_models();
    }

    fn load_models() -> Vec<ModelEntry> {
        let config_path = llm::provider_config::ConfigurationSystem::models_config_path();
        let content = match std::fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let map: BTreeMap<String, serde_json::Value> = match serde_json::from_str(&content) {
            Ok(m) => m,
            Err(e) => {
                warn!("Failed to parse models.json: {}", e);
                return Vec::new();
            }
        };

        map.into_iter()
            .map(|(name, value)| {
                let provider = value
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let model_id = value
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let context_limit = value
                    .get("context_token_limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;

                ModelEntry {
                    name,
                    provider,
                    model_id,
                    context_limit,
                }
            })
            .collect()
    }

    fn render_model_card(&self, entry: &ModelEntry, cx: &mut Context<Self>) -> impl IntoElement {
        let context_str = if entry.context_limit >= 1000 {
            format!("{}K context", entry.context_limit / 1000)
        } else {
            format!("{} tokens", entry.context_limit)
        };

        div()
            .id(SharedString::from(format!("model-{}", entry.name)))
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            // Info
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(cx.theme().foreground)
                            .child(SharedString::from(entry.name.clone())),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(format!(
                                        "Provider: {}",
                                        entry.provider
                                    ))),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(format!("· {}", context_str))),
                            ),
                    )
                    .when(!entry.model_id.is_empty(), |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground.opacity(0.7))
                                .child(SharedString::from(format!("ID: {}", entry.model_id))),
                        )
                    }),
            )
    }
}

impl Focusable for ModelsSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ModelsSection {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .max_w(px(700.))
            // Header
            .child(
                div().flex().items_center().justify_between().child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .child("MODELS"),
                ),
            )
            // Model cards
            .children(
                self.models
                    .clone()
                    .iter()
                    .map(|entry| self.render_model_card(entry, cx).into_any_element()),
            )
            // Empty state
            .when(self.models.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .py_8()
                        .gap_3()
                        .child(
                            div()
                                .text_base()
                                .text_color(cx.theme().muted_foreground)
                                .child("No models configured yet"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Add a provider first, then configure models that use it."),
                        ),
                )
            })
    }
}
