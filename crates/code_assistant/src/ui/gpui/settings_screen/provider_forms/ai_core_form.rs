//! AI Core provider form: service key paste + deployment mapping editor.
//!
//! The SAP AI Core provider has a unique configuration that includes:
//! - OAuth2 credentials (client_id, client_secret, token_url) from a service key
//! - An API base URL
//! - A mapping of model IDs to deployment UUIDs with API type selection
//!
//! The UX allows pasting the entire service key JSON to auto-fill credentials,
//! then managing individual deployment mappings.

use super::ProviderForm;
use gpui::{div, prelude::*, px, App, Context, Entity, SharedString, Window};
use gpui_component::input::{Input, InputState};
use gpui_component::ActiveTheme;
use serde_json::Value;
use tracing::warn;

/// A single model-to-deployment mapping entry.
#[derive(Clone, Debug)]
pub struct DeploymentEntry {
    /// The model ID (user-defined, matches models.json)
    pub model_id: String,
    /// The deployment UUID from AI Core
    pub deployment_id: String,
    /// Which API type this deployment uses
    pub api_type: String,
}

pub struct AiCoreProviderForm {
    // Credentials (auto-filled from service key or manually set)
    client_id_input: Entity<InputState>,
    client_secret_input: Entity<InputState>,
    token_url_input: Entity<InputState>,
    api_base_url_input: Entity<InputState>,

    // Service key paste area
    service_key_input: Entity<InputState>,
    /// Status message after parsing service key
    service_key_status: Option<(bool, String)>,

    // Deployment mappings
    deployments: Vec<DeploymentEntry>,

    // Input states for the "add deployment" row
    new_deployment_model_input: Entity<InputState>,
    new_deployment_id_input: Entity<InputState>,
    new_deployment_api_type: String,
}

impl AiCoreProviderForm {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let client_id_input = cx.new(|cx| InputState::new(window, cx).placeholder("client_id"));
        let client_secret_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("client_secret"));
        let token_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://...authentication.sap.hana.ondemand.com/oauth/token")
        });
        let api_base_url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://api.ai.<region>.aws.ml.hana.ondemand.com/v2/inference")
        });
        let service_key_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Paste service key JSON here..."));
        let new_deployment_model_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("model-id"));
        let new_deployment_id_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("deployment UUID"));

        Self {
            client_id_input,
            client_secret_input,
            token_url_input,
            api_base_url_input,
            service_key_input,
            service_key_status: None,
            deployments: Vec::new(),
            new_deployment_model_input,
            new_deployment_id_input,
            new_deployment_api_type: "anthropic".to_string(),
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
            .child(div().flex_1().child(widget))
    }

    /// Parse the service key JSON and populate credential fields.
    fn apply_service_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.service_key_input.read(cx).value().to_string();
        if raw.trim().is_empty() {
            self.service_key_status = Some((false, "No input provided".to_string()));
            cx.notify();
            return;
        }

        let parsed: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                self.service_key_status = Some((false, format!("Invalid JSON: {}", e)));
                cx.notify();
                return;
            }
        };

        let client_id = parsed
            .get("clientid")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let client_secret = parsed
            .get("clientsecret")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let auth_url = parsed.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let api_url = parsed
            .get("serviceurls")
            .and_then(|v| v.get("AI_API_URL"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if client_id.is_empty() || client_secret.is_empty() {
            self.service_key_status = Some((
                false,
                "Missing 'clientid' or 'clientsecret' in service key".to_string(),
            ));
            cx.notify();
            return;
        }

        let token_url = if auth_url.is_empty() {
            String::new()
        } else {
            format!("{}/oauth/token", auth_url.trim_end_matches('/'))
        };

        let inference_url = if api_url.is_empty() {
            String::new()
        } else {
            let base = api_url.trim_end_matches('/');
            if base.ends_with("/inference") {
                base.to_string()
            } else {
                format!("{}/inference", base)
            }
        };

        self.client_id_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(client_id.to_string()), window, cx);
        });
        self.client_secret_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(client_secret.to_string()), window, cx);
        });
        self.token_url_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(token_url), window, cx);
        });
        self.api_base_url_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(inference_url), window, cx);
        });

        let mut extracted = Vec::new();
        if !client_id.is_empty() {
            extracted.push("client_id");
        }
        if !client_secret.is_empty() {
            extracted.push("client_secret");
        }
        if !auth_url.is_empty() {
            extracted.push("token_url");
        }
        if !api_url.is_empty() {
            extracted.push("api_base_url");
        }

        self.service_key_status = Some((true, format!("Extracted: {}", extracted.join(", "))));

        // Clear input after success
        self.service_key_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(""), window, cx);
        });

        cx.notify();
    }

    /// Add a new deployment entry.
    fn add_deployment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model_id = self.new_deployment_model_input.read(cx).value().to_string();
        let deployment_id = self.new_deployment_id_input.read(cx).value().to_string();

        if model_id.trim().is_empty() || deployment_id.trim().is_empty() {
            return;
        }

        self.deployments.push(DeploymentEntry {
            model_id: model_id.trim().to_string(),
            deployment_id: deployment_id.trim().to_string(),
            api_type: self.new_deployment_api_type.clone(),
        });

        self.new_deployment_model_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(""), window, cx);
        });
        self.new_deployment_id_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(""), window, cx);
        });
        self.new_deployment_api_type = "anthropic".to_string();
        cx.notify();
    }

    /// Remove a deployment entry by index.
    fn remove_deployment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.deployments.len() {
            self.deployments.remove(index);
            cx.notify();
        }
    }
}

impl ProviderForm for AiCoreProviderForm {
    fn to_config_json(&self, cx: &App) -> serde_json::Map<String, Value> {
        let mut config = serde_json::Map::new();

        let client_id = self.client_id_input.read(cx).value().to_string();
        let client_secret = self.client_secret_input.read(cx).value().to_string();
        let token_url = self.token_url_input.read(cx).value().to_string();
        let api_base_url = self.api_base_url_input.read(cx).value().to_string();

        if !client_id.is_empty() {
            config.insert("client_id".to_string(), Value::String(client_id));
        }
        if !client_secret.is_empty() {
            config.insert("client_secret".to_string(), Value::String(client_secret));
        }
        if !token_url.is_empty() {
            config.insert("token_url".to_string(), Value::String(token_url));
        }
        if !api_base_url.is_empty() {
            config.insert("api_base_url".to_string(), Value::String(api_base_url));
        }

        if !self.deployments.is_empty() {
            let mut models = serde_json::Map::new();
            for entry in &self.deployments {
                if entry.api_type == "anthropic" {
                    models.insert(
                        entry.model_id.clone(),
                        Value::String(entry.deployment_id.clone()),
                    );
                } else {
                    let mut obj = serde_json::Map::new();
                    obj.insert(
                        "deployment".to_string(),
                        Value::String(entry.deployment_id.clone()),
                    );
                    obj.insert(
                        "api_type".to_string(),
                        Value::String(entry.api_type.clone()),
                    );
                    models.insert(entry.model_id.clone(), Value::Object(obj));
                }
            }
            config.insert("models".to_string(), Value::Object(models));
        }

        config
    }

    fn load_config(
        &mut self,
        config: &serde_json::Map<String, Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(v) = config.get("client_id").and_then(|v| v.as_str()) {
            self.client_id_input.update(cx, |state, cx| {
                state.set_value(SharedString::from(v.to_string()), window, cx);
            });
        }
        if let Some(v) = config.get("client_secret").and_then(|v| v.as_str()) {
            self.client_secret_input.update(cx, |state, cx| {
                state.set_value(SharedString::from(v.to_string()), window, cx);
            });
        }
        if let Some(v) = config.get("token_url").and_then(|v| v.as_str()) {
            self.token_url_input.update(cx, |state, cx| {
                state.set_value(SharedString::from(v.to_string()), window, cx);
            });
        }
        if let Some(v) = config.get("api_base_url").and_then(|v| v.as_str()) {
            self.api_base_url_input.update(cx, |state, cx| {
                state.set_value(SharedString::from(v.to_string()), window, cx);
            });
        }

        self.deployments.clear();
        if let Some(models) = config.get("models").and_then(|v| v.as_object()) {
            for (model_id, value) in models {
                let (deployment_id, api_type) = if let Some(uuid) = value.as_str() {
                    (uuid.to_string(), "anthropic".to_string())
                } else if let Some(obj) = value.as_object() {
                    let dep = obj
                        .get("deployment")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let api = obj
                        .get("api_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("anthropic")
                        .to_string();
                    (dep, api)
                } else {
                    warn!("Skipping invalid deployment for model '{}'", model_id);
                    continue;
                };

                self.deployments.push(DeploymentEntry {
                    model_id: model_id.clone(),
                    deployment_id,
                    api_type,
                });
            }
        }

        cx.notify();
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.client_id_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(""), window, cx);
        });
        self.client_secret_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(""), window, cx);
        });
        self.token_url_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(""), window, cx);
        });
        self.api_base_url_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(""), window, cx);
        });
        self.service_key_input.update(cx, |state, cx| {
            state.set_value(SharedString::from(""), window, cx);
        });
        self.deployments.clear();
        self.service_key_status = None;
        self.new_deployment_api_type = "anthropic".to_string();
        cx.notify();
    }
}

impl Render for AiCoreProviderForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.service_key_status.clone();
        let deployments = self.deployments.clone();

        div()
            .flex()
            .flex_col()
            .gap_2()
            // Service key quick-setup (compact inline)
            .child(
                div()
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
                            .child("Service Key"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.service_key_input)))
                            .child(
                                div()
                                    .id("apply-service-key")
                                    .flex_none()
                                    .px_2()
                                    .py(px(4.))
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_xs()
                                    .bg(cx.theme().primary)
                                    .text_color(cx.theme().primary_foreground)
                                    .hover(|s| s.opacity(0.9))
                                    .child("Apply")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.apply_service_key(window, cx);
                                    })),
                            ),
                    ),
            )
            // Status / help text
            .child(
                div().pl(px(83.)).child(match status {
                    Some((true, msg)) => div()
                        .text_xs()
                        .text_color(gpui::hsla(142.0 / 360.0, 0.7, 0.45, 1.0))
                        .child(SharedString::from(msg)),
                    Some((false, msg)) => div()
                        .text_xs()
                        .text_color(gpui::hsla(0.0, 0.7, 0.5, 1.0))
                        .child(SharedString::from(msg)),
                    None => div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Paste BTP service key JSON to auto-fill credentials below"),
                }),
            )
            // Credential fields
            .child(self.form_row(
                "Client ID",
                Input::new(&self.client_id_input).into_any_element(),
                cx,
            ))
            .child(self.form_row(
                "Secret",
                Input::new(&self.client_secret_input).into_any_element(),
                cx,
            ))
            .child(self.form_row(
                "Token URL",
                Input::new(&self.token_url_input).into_any_element(),
                cx,
            ))
            .child(self.form_row(
                "API URL",
                Input::new(&self.api_base_url_input).into_any_element(),
                cx,
            ))
            // Deployments section
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .mt_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    // Header
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .w(px(80.))
                                    .flex_none()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .child("Deployments"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Map model IDs to AI Core deployment UUIDs"),
                            ),
                    )
                    // Existing entries
                    .children(deployments.iter().enumerate().map(|(idx, entry)| {
                        let entry_clone = entry.clone();
                        div()
                            .id(SharedString::from(format!("deployment-{}", idx)))
                            .flex()
                            .items_center()
                            .gap_2()
                            .ml(px(83.))
                            .py_1()
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child(SharedString::from(entry_clone.model_id)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("→"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .overflow_x_hidden()
                                    .text_ellipsis()
                                    .child(SharedString::from(entry_clone.deployment_id)),
                            )
                            .child(
                                div()
                                    .px_1()
                                    .py(px(1.))
                                    .rounded(px(3.))
                                    .text_xs()
                                    .bg(cx.theme().muted)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(entry_clone.api_type)),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("del-dep-{}", idx)))
                                    .flex_none()
                                    .px_1()
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(gpui::hsla(0.0, 0.7, 0.5, 1.0))
                                    .hover(|s| s.text_color(gpui::hsla(0.0, 0.8, 0.4, 1.0)))
                                    .child("×")
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.remove_deployment(idx, cx);
                                    })),
                            )
                            .into_any_element()
                    }))
                    // Add new deployment row
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .ml(px(83.))
                            .mt_1()
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&self.new_deployment_model_input)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&self.new_deployment_id_input)),
                            )
                            // API type mini-selector
                            .child(div().flex().gap_1().children(
                                ["anthropic", "openai", "vertex"].iter().map(|&api_type| {
                                    let is_selected = self.new_deployment_api_type == api_type;
                                    let api_type_owned = api_type.to_string();
                                    let short_label = match api_type {
                                        "anthropic" => "A",
                                        "openai" => "O",
                                        "vertex" => "V",
                                        _ => "?",
                                    };
                                    div()
                                        .id(SharedString::from(format!("api-type-{}", api_type)))
                                        .size(px(20.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(3.))
                                        .cursor_pointer()
                                        .text_xs()
                                        .when(is_selected, |s| {
                                            s.bg(cx.theme().primary.opacity(0.2))
                                                .text_color(cx.theme().primary)
                                        })
                                        .when(!is_selected, |s| {
                                            s.text_color(cx.theme().muted_foreground)
                                                .hover(|s| s.bg(cx.theme().muted))
                                        })
                                        .child(short_label)
                                        .on_click(cx.listener(move |this, _, _window, cx| {
                                            this.new_deployment_api_type = api_type_owned.clone();
                                            cx.notify();
                                        }))
                                        .into_any_element()
                                }),
                            ))
                            // Add button
                            .child(
                                div()
                                    .id("add-deployment-btn")
                                    .flex_none()
                                    .px_1()
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(gpui::hsla(142.0 / 360.0, 0.7, 0.45, 1.0))
                                    .hover(|s| {
                                        s.text_color(gpui::hsla(142.0 / 360.0, 0.8, 0.35, 1.0))
                                    })
                                    .child("+")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_deployment(window, cx);
                                    })),
                            ),
                    ),
            )
    }
}
