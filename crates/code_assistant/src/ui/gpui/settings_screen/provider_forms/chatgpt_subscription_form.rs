//! ChatGPT Subscription provider form: OAuth2 browser login flow.
//!
//! Instead of a base_url + api_key form, this provider uses the OpenAI Codex
//! OAuth2 PKCE flow to authenticate with a ChatGPT Plus/Pro/Team subscription.
//!
//! The form shows:
//! - When not logged in: a "Login with OpenAI" button
//! - When logged in: email, plan type, token status, and logout/re-login buttons

use super::ProviderForm;
use gpui::{div, prelude::*, px, App, Context, SharedString, Task, Window};
use gpui_component::ActiveTheme;
use llm::codex_auth::{self, CodexAuthState, CodexTokenStorage, ProvidersJsonTokenStorage};
use serde_json::Value;
use std::sync::Arc;
use tracing::{info, warn};

/// Authentication status displayed in the form.
#[derive(Clone, Debug)]
enum AuthStatus {
    /// No tokens present; user needs to log in.
    NotAuthenticated,
    /// Login flow is in progress (browser opened, waiting for callback).
    LoginInProgress,
    /// Successfully authenticated.
    Authenticated {
        email: Option<String>,
        plan_type: Option<String>,
        needs_refresh: bool,
    },
    /// An error occurred during login or logout.
    Error(String),
}

pub struct ChatGptSubscriptionForm {
    /// Current auth status derived from stored tokens.
    auth_status: AuthStatus,
    /// The provider key/id used for token storage (set when editing an existing provider).
    provider_id: Option<String>,
    /// Stored auth state (if authenticated).
    auth_state: Option<CodexAuthState>,
    /// Background login task (kept alive to avoid cancellation).
    _login_task: Option<Task<()>>,
}

impl ChatGptSubscriptionForm {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            auth_status: AuthStatus::NotAuthenticated,
            provider_id: None,
            auth_state: None,
            _login_task: None,
        }
    }

    /// Get or create the token storage backend.
    fn storage(&self) -> Arc<dyn CodexTokenStorage> {
        let provider_id = self
            .provider_id
            .clone()
            .unwrap_or_else(|| codex_auth::DEFAULT_PROVIDER_ID.to_string());
        Arc::new(ProvidersJsonTokenStorage::new(provider_id, None))
    }

    /// Start the OAuth login flow: open browser and wait for callback.
    fn start_login(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.auth_status = AuthStatus::LoginInProgress;
        cx.notify();

        let storage = self.storage();

        let task = cx.spawn(async move |this, cx| {
            // Start the login flow (spawns local server, returns URL)
            let result = codex_auth::start_login_flow(storage).await;

            match result {
                Ok((authorize_url, rx)) => {
                    // Open the browser
                    if let Err(e) = open::that(&authorize_url) {
                        warn!("Could not open browser: {e}");
                    }
                    info!("Opened browser for OpenAI login");

                    // Wait for the callback (up to 5 min timeout is in the server)
                    match rx.await {
                        Ok(Ok(login_result)) => {
                            let auth_state = login_result.auth_state.clone();
                            let status = codex_auth::get_auth_status_from_state(&auth_state);
                            let _ = cx.update(|cx| {
                                this.update(cx, |this, cx| {
                                    this.auth_state = Some(auth_state);
                                    this.auth_status = AuthStatus::Authenticated {
                                        email: status.email,
                                        plan_type: status.plan_type,
                                        needs_refresh: status.needs_refresh,
                                    };
                                    cx.notify();
                                })
                            });
                        }
                        Ok(Err(e)) => {
                            let msg = format!("{e}");
                            let _ = cx.update(|cx| {
                                this.update(cx, |this, cx| {
                                    this.auth_status = AuthStatus::Error(msg);
                                    cx.notify();
                                })
                            });
                        }
                        Err(_) => {
                            let _ = cx.update(|cx| {
                                this.update(cx, |this, cx| {
                                    this.auth_status =
                                        AuthStatus::Error("Login cancelled".to_string());
                                    cx.notify();
                                })
                            });
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("{e}");
                    let _ = cx.update(|cx| {
                        this.update(cx, |this, cx| {
                            this.auth_status = AuthStatus::Error(msg);
                            cx.notify();
                        })
                    });
                }
            }
        });

        self._login_task = Some(task);
    }

    /// Clear stored tokens (logout).
    fn logout(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let storage = self.storage();
        if let Err(e) = storage.delete() {
            warn!("Failed to delete tokens: {e}");
            self.auth_status = AuthStatus::Error(format!("Logout failed: {e}"));
        } else {
            self.auth_state = None;
            self.auth_status = AuthStatus::NotAuthenticated;
            info!("Logged out from ChatGPT subscription");
        }
        cx.notify();
    }

    /// Render the "not authenticated" state with login button.
    fn render_not_authenticated(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        "Use your existing ChatGPT Plus, Pro, or Team subscription. \
                         Click the button below to sign in with your OpenAI account.",
                    ),
            )
            .child(
                div()
                    .id("chatgpt-login-btn")
                    .px_3()
                    .py(px(6.))
                    .rounded_md()
                    .cursor_pointer()
                    .text_xs()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .bg(cx.theme().primary)
                    .text_color(cx.theme().primary_foreground)
                    .hover(|s| s.opacity(0.9))
                    .child("Login with OpenAI")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.start_login(window, cx);
                    })),
            )
    }

    /// Render the "login in progress" state.
    fn render_login_in_progress(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Waiting for authentication in your browser..."),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .italic()
                    .child("A browser window should have opened. Complete the login there."),
            )
    }

    /// Render the "authenticated" state with user info.
    fn render_authenticated(
        &self,
        email: &Option<String>,
        plan_type: &Option<String>,
        needs_refresh: &bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let email_display = email.clone().unwrap_or_else(|| "(unknown)".to_string());
        let plan_display = plan_type.clone().unwrap_or_else(|| "(unknown)".to_string());
        let needs_refresh = *needs_refresh;

        div()
            .flex()
            .flex_col()
            .gap_2()
            // Status badge
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(8.))
                            .h(px(8.))
                            .rounded_full()
                            .bg(if needs_refresh {
                                gpui::hsla(0.12, 0.8, 0.5, 1.0) // orange
                            } else {
                                gpui::hsla(0.33, 0.7, 0.4, 1.0) // green
                            }),
                    )
                    .child(div().text_xs().font_weight(gpui::FontWeight::MEDIUM).child(
                        if needs_refresh {
                            SharedString::from("Authenticated (token refresh recommended)")
                        } else {
                            SharedString::from("Authenticated")
                        },
                    )),
            )
            // User info rows
            .child(self.render_info_row("Email", &email_display, cx))
            .child(self.render_info_row("Plan", &plan_display, cx))
            // Action buttons
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .mt_1()
                    .child(
                        div()
                            .id("chatgpt-logout-btn")
                            .px_3()
                            .py(px(6.))
                            .rounded_md()
                            .cursor_pointer()
                            .text_xs()
                            .text_color(gpui::hsla(0.0, 0.7, 0.5, 1.0))
                            .hover(|s| s.bg(gpui::hsla(0.0, 0.7, 0.5, 0.1)))
                            .child("Logout")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.logout(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .id("chatgpt-relogin-btn")
                            .px_3()
                            .py(px(6.))
                            .rounded_md()
                            .cursor_pointer()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .hover(|s| s.bg(cx.theme().muted))
                            .child("Re-authenticate")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start_login(window, cx);
                            })),
                    ),
            )
    }

    /// Render the error state.
    fn render_error(&self, message: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let message = message.to_string();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(gpui::hsla(0.0, 0.7, 0.5, 1.0))
                    .child(SharedString::from(format!("Error: {}", message))),
            )
            .child(
                div()
                    .id("chatgpt-retry-btn")
                    .px_3()
                    .py(px(6.))
                    .rounded_md()
                    .cursor_pointer()
                    .text_xs()
                    .bg(cx.theme().primary)
                    .text_color(cx.theme().primary_foreground)
                    .hover(|s| s.opacity(0.9))
                    .child("Try Again")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.start_login(window, cx);
                    })),
            )
    }

    /// Render an info row (label: value).
    fn render_info_row(
        &self,
        label: &str,
        value: &str,
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
            .child(div().text_xs().child(SharedString::from(value.to_string())))
    }
}

impl ProviderForm for ChatGptSubscriptionForm {
    fn to_config_json(&self, _cx: &App) -> serde_json::Map<String, Value> {
        let mut config = serde_json::Map::new();
        config.insert("codex_auth".to_string(), Value::Bool(true));

        // Include the tokens if we have them (from a successful login during this session)
        if let Some(ref state) = self.auth_state {
            let tokens_value = serde_json::json!({
                "id_token": state.tokens.id_token,
                "access_token": state.tokens.access_token,
                "refresh_token": state.tokens.refresh_token,
                "account_id": state.tokens.account_id,
                "last_refresh": state.last_refresh,
            });
            config.insert("codex_tokens".to_string(), tokens_value);
        }

        config
    }

    fn load_config(
        &mut self,
        config: &serde_json::Map<String, Value>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Try to load auth state from the config
        let config_value = Value::Object(config.clone());
        if let Some(state) = codex_auth::load_auth_state_from_config(&config_value) {
            let status = codex_auth::get_auth_status_from_state(&state);
            self.auth_state = Some(state);
            self.auth_status = AuthStatus::Authenticated {
                email: status.email,
                plan_type: status.plan_type,
                needs_refresh: status.needs_refresh,
            };
        } else {
            self.auth_state = None;
            self.auth_status = AuthStatus::NotAuthenticated;
        }
        cx.notify();
    }

    fn reset(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.auth_status = AuthStatus::NotAuthenticated;
        self.auth_state = None;
        self.provider_id = None;
        self._login_task = None;
        cx.notify();
    }
}

impl Render for ChatGptSubscriptionForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.auth_status {
            AuthStatus::NotAuthenticated => self.render_not_authenticated(cx).into_any_element(),
            AuthStatus::LoginInProgress => self.render_login_in_progress(cx).into_any_element(),
            AuthStatus::Authenticated {
                email,
                plan_type,
                needs_refresh,
            } => self
                .render_authenticated(email, plan_type, needs_refresh, cx)
                .into_any_element(),
            AuthStatus::Error(msg) => self.render_error(msg, cx).into_any_element(),
        };

        div().flex().flex_col().child(content)
    }
}
