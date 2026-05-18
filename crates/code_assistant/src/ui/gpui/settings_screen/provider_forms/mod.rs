//! Provider-specific form configurations.
//!
//! Each provider type can define its own UI form for configuration.
//! Form entities implement Render so they can be included as children
//! in the parent providers section view.

pub mod ai_core_form;
pub mod chatgpt_subscription_form;
pub mod default_form;

use gpui::{AnyElement, App, AppContext as _, Context, Entity, IntoElement, Window};
use serde_json::Value;

/// Trait for provider-specific configuration forms.
///
/// Each provider type (anthropic, openai, ai-core, etc.) implements
/// a form entity that renders its own UI and knows how to produce a
/// config JSON object for saving to providers.json.
pub trait ProviderForm: 'static + Sized {
    /// Extract the config JSON from the current form state.
    /// Returns the contents of the "config" object in providers.json.
    fn to_config_json(&self, cx: &App) -> serde_json::Map<String, Value>;

    /// Populate the form from an existing config JSON.
    fn load_config(
        &mut self,
        config: &serde_json::Map<String, Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    );

    /// Reset the form to empty/default state.
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>);
}

/// Factory function to determine which form type to use for a provider.
pub fn form_type_for_provider(provider_type: &str) -> ProviderFormType {
    match provider_type {
        "ai-core" => ProviderFormType::AiCore,
        "openai-responses-ws" => ProviderFormType::ChatGptSubscription,
        _ => ProviderFormType::Default,
    }
}

/// Which form UI to use for a provider type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderFormType {
    /// Standard form: base_url + api_key
    Default,
    /// AI Core form: service key paste + deployment mapping
    AiCore,
    /// ChatGPT Subscription: OAuth2 browser login
    ChatGptSubscription,
}

/// A wrapper that can hold any provider form entity and delegate to it.
/// This allows the providers section to work with a single field regardless
/// of which form type is active.
pub struct ProviderFormHolder {
    pub form_type: ProviderFormType,
    pub default_form: Option<Entity<default_form::DefaultProviderForm>>,
    pub ai_core_form: Option<Entity<ai_core_form::AiCoreProviderForm>>,
    pub chatgpt_form: Option<Entity<chatgpt_subscription_form::ChatGptSubscriptionForm>>,
}

impl ProviderFormHolder {
    pub fn new(
        provider_type: &str,
        window: &mut Window,
        cx: &mut Context<super::providers_section::ProvidersSection>,
    ) -> Self {
        let form_type = form_type_for_provider(provider_type);
        match form_type {
            ProviderFormType::Default => Self {
                form_type,
                default_form: Some(cx.new(|cx| default_form::DefaultProviderForm::new(window, cx))),
                ai_core_form: None,
                chatgpt_form: None,
            },
            ProviderFormType::AiCore => Self {
                form_type,
                default_form: None,
                ai_core_form: Some(cx.new(|cx| ai_core_form::AiCoreProviderForm::new(window, cx))),
                chatgpt_form: None,
            },
            ProviderFormType::ChatGptSubscription => {
                Self {
                    form_type,
                    default_form: None,
                    ai_core_form: None,
                    chatgpt_form: Some(cx.new(|cx| {
                        chatgpt_subscription_form::ChatGptSubscriptionForm::new(window, cx)
                    })),
                }
            }
        }
    }

    /// Switch the form type, creating a new form entity if needed.
    pub fn switch_to(
        &mut self,
        provider_type: &str,
        window: &mut Window,
        cx: &mut Context<super::providers_section::ProvidersSection>,
    ) {
        let new_type = form_type_for_provider(provider_type);
        if new_type == self.form_type {
            return;
        }
        self.form_type = new_type;
        match new_type {
            ProviderFormType::Default => {
                if self.default_form.is_none() {
                    self.default_form =
                        Some(cx.new(|cx| default_form::DefaultProviderForm::new(window, cx)));
                }
                self.ai_core_form = None;
                self.chatgpt_form = None;
            }
            ProviderFormType::AiCore => {
                if self.ai_core_form.is_none() {
                    self.ai_core_form =
                        Some(cx.new(|cx| ai_core_form::AiCoreProviderForm::new(window, cx)));
                }
                self.default_form = None;
                self.chatgpt_form = None;
            }
            ProviderFormType::ChatGptSubscription => {
                if self.chatgpt_form.is_none() {
                    self.chatgpt_form = Some(cx.new(|cx| {
                        chatgpt_subscription_form::ChatGptSubscriptionForm::new(window, cx)
                    }));
                }
                self.default_form = None;
                self.ai_core_form = None;
            }
        }
    }

    /// Render the active form as an AnyElement. Since form entities implement Render,
    /// we just return them as elements.
    pub fn render(&self) -> AnyElement {
        match self.form_type {
            ProviderFormType::Default => {
                if let Some(form) = &self.default_form {
                    form.clone().into_any_element()
                } else {
                    gpui::Empty.into_any_element()
                }
            }
            ProviderFormType::AiCore => {
                if let Some(form) = &self.ai_core_form {
                    form.clone().into_any_element()
                } else {
                    gpui::Empty.into_any_element()
                }
            }
            ProviderFormType::ChatGptSubscription => {
                if let Some(form) = &self.chatgpt_form {
                    form.clone().into_any_element()
                } else {
                    gpui::Empty.into_any_element()
                }
            }
        }
    }

    /// Get config JSON from the active form.
    pub fn to_config_json(&self, cx: &App) -> serde_json::Map<String, Value> {
        match self.form_type {
            ProviderFormType::Default => {
                if let Some(form) = &self.default_form {
                    form.read(cx).to_config_json(cx)
                } else {
                    serde_json::Map::new()
                }
            }
            ProviderFormType::AiCore => {
                if let Some(form) = &self.ai_core_form {
                    form.read(cx).to_config_json(cx)
                } else {
                    serde_json::Map::new()
                }
            }
            ProviderFormType::ChatGptSubscription => {
                if let Some(form) = &self.chatgpt_form {
                    form.read(cx).to_config_json(cx)
                } else {
                    serde_json::Map::new()
                }
            }
        }
    }

    /// Populate the active form from config JSON.
    pub fn load_config(
        &self,
        config: &serde_json::Map<String, Value>,
        window: &mut Window,
        cx: &mut Context<super::providers_section::ProvidersSection>,
    ) {
        match self.form_type {
            ProviderFormType::Default => {
                if let Some(form) = &self.default_form {
                    form.update(cx, |form, cx| form.load_config(config, window, cx));
                }
            }
            ProviderFormType::AiCore => {
                if let Some(form) = &self.ai_core_form {
                    form.update(cx, |form, cx| form.load_config(config, window, cx));
                }
            }
            ProviderFormType::ChatGptSubscription => {
                if let Some(form) = &self.chatgpt_form {
                    form.update(cx, |form, cx| form.load_config(config, window, cx));
                }
            }
        }
    }

    /// Reset the active form.
    pub fn reset(
        &self,
        window: &mut Window,
        cx: &mut Context<super::providers_section::ProvidersSection>,
    ) {
        match self.form_type {
            ProviderFormType::Default => {
                if let Some(form) = &self.default_form {
                    form.update(cx, |form, cx| form.reset(window, cx));
                }
            }
            ProviderFormType::AiCore => {
                if let Some(form) = &self.ai_core_form {
                    form.update(cx, |form, cx| form.reset(window, cx));
                }
            }
            ProviderFormType::ChatGptSubscription => {
                if let Some(form) = &self.chatgpt_form {
                    form.update(cx, |form, cx| form.reset(window, cx));
                }
            }
        }
    }
}
