//! Default provider form: base_url + api_key fields.
//!
//! Used for most providers (Anthropic, OpenAI, Ollama, etc.)

use super::ProviderForm;
use gpui::{div, prelude::*, px, App, Context, Entity, SharedString, Window};
use gpui_component::input::{Input, InputState};
use gpui_component::ActiveTheme;
use serde_json::Value;

pub struct DefaultProviderForm {
    pub base_url_input: Entity<InputState>,
    pub api_key_input: Entity<InputState>,
    /// Whether we're in edit mode (affects placeholder text for API key)
    pub is_editing: bool,
}

impl DefaultProviderForm {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let base_url_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://api.example.com/v1"));
        let api_key_input = cx.new(|cx| InputState::new(window, cx).placeholder("sk-..."));

        Self {
            base_url_input,
            api_key_input,
            is_editing: false,
        }
    }

    /// Render a form row with label on left, widget on right.
    fn form_row(
        &self,
        label: &str,
        widget: gpui::AnyElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .w_full()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .w(px(80.))
                    .flex_none()
                    .text_xs()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(label.to_string())),
            )
            .child(div().flex_1().min_w_0().child(widget))
    }
}

impl ProviderForm for DefaultProviderForm {
    fn to_config_json(&self, cx: &App) -> serde_json::Map<String, Value> {
        let mut config = serde_json::Map::new();

        let base_url = self.base_url_input.read(cx).value().to_string();
        let api_key = self.api_key_input.read(cx).value().to_string();

        if !base_url.is_empty() {
            config.insert("base_url".to_string(), Value::String(base_url));
        }
        if !api_key.is_empty() {
            config.insert("api_key".to_string(), Value::String(api_key));
        }

        config
    }

    fn load_config(
        &mut self,
        config: &serde_json::Map<String, Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_editing = true;

        if let Some(base_url) = config.get("base_url").and_then(|v| v.as_str()) {
            self.base_url_input.update(cx, |state, cx| {
                state.set_value(SharedString::from(base_url.to_string()), window, cx);
            });
        }
        // Don't populate API key for security
        self.api_key_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(""), window, cx);
        });
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.is_editing = false;
        self.base_url_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(""), window, cx);
        });
        self.api_key_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(""), window, cx);
        });
    }
}

impl Render for DefaultProviderForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let api_key_label = if self.is_editing {
            "API Key *"
        } else {
            "API Key"
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.form_row(
                "Base URL",
                Input::new(&self.base_url_input).into_any_element(),
                cx,
            ))
            .child(self.form_row(
                api_key_label,
                Input::new(&self.api_key_input).into_any_element(),
                cx,
            ))
            .when(self.is_editing, |el| {
                el.child(
                    div()
                        .pl(px(83.))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("* Leave empty to keep current key"),
                )
            })
    }
}
