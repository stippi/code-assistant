//! Voice settings: the realtime model voice mode talks to, and when
//! conversations report back. Stored in `voice.json`.

use code_assistant_core::voice::config::supports_realtime;
use code_assistant_core::voice::{NotifyScope, VoiceConfig};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable, Size};
use gpui_kit::{
    App, Context, Entity, FocusHandle, Focusable, SharedString, Subscription, div, prelude::*, px,
};
use tracing::warn;

const EAGERNESS: &[&str] = &["auto", "low", "medium", "high"];

/// Which text field an input edits.
#[derive(Clone, Copy)]
enum Field {
    Model,
    Voice,
    Transcription,
    Cooling,
}

pub struct VoiceSection {
    focus_handle: FocusHandle,
    config: VoiceConfig,
    /// (provider id, label) of the providers with a realtime API.
    providers: Vec<(String, String)>,
    model_input: Entity<InputState>,
    voice_input: Entity<InputState>,
    transcription_input: Entity<InputState>,
    cooling_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl VoiceSection {
    pub fn new(window: &mut gpui_kit::Window, cx: &mut Context<Self>) -> Self {
        let config = VoiceConfig::load();
        let mut subscriptions = Vec::new();
        let mut input = |value: String, placeholder: &str, field: Field, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            let state = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            });
            subscriptions.push(cx.subscribe_in(
                &state,
                window,
                move |this, input, event: &InputEvent, _window, cx| {
                    if matches!(event, InputEvent::Change) {
                        let value = input.read(cx).value().trim().to_string();
                        this.on_field_changed(field, value, cx);
                    }
                },
            ));
            state
        };
        let model_input = input(config.model.clone(), "gpt-realtime", Field::Model, cx);
        let voice_input = input(config.voice.clone(), "marin", Field::Voice, cx);
        let transcription_input = input(
            config.transcription_model.clone(),
            "gpt-4o-mini-transcribe",
            Field::Transcription,
            cx,
        );
        let cooling_input = input(config.cooling_ms.to_string(), "1500", Field::Cooling, cx);
        Self {
            focus_handle: cx.focus_handle(),
            config,
            providers: realtime_providers(),
            model_input,
            voice_input,
            transcription_input,
            cooling_input,
            _subscriptions: subscriptions,
        }
    }

    /// Re-read the providers (they may have changed in another section).
    pub fn reload(&mut self) {
        self.providers = realtime_providers();
        self.config = VoiceConfig::load();
    }

    fn on_field_changed(&mut self, field: Field, value: String, cx: &App) {
        match field {
            Field::Model => self.config.model = value,
            Field::Voice => self.config.voice = value,
            Field::Transcription => self.config.transcription_model = value,
            Field::Cooling => {
                let Ok(ms) = value.parse() else { return };
                self.config.cooling_ms = ms;
            }
        }
        self.save(cx);
    }

    fn update(&mut self, change: impl FnOnce(&mut VoiceConfig), cx: &mut Context<Self>) {
        change(&mut self.config);
        self.save(cx);
        cx.notify();
    }

    fn save(&self, cx: &App) {
        if let Err(e) = self.config.save() {
            warn!("Failed to save the voice settings: {e:#}");
        }
        // The title bar shows voice mode only once it is configured.
        if let Some(gpui) = cx.try_global::<crate::Gpui>() {
            gpui.refresh_voice_configured();
        }
    }

    fn render_choice(
        &self,
        id: String,
        label: String,
        selected: bool,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(id))
            .px_3()
            .py_1()
            .rounded_md()
            .border_1()
            .cursor_pointer()
            .text_sm()
            .border_color(if selected {
                cx.theme().primary
            } else {
                cx.theme().border
            })
            .text_color(if selected {
                cx.theme().foreground
            } else {
                cx.theme().muted_foreground
            })
            .when(selected, |el| el.bg(cx.theme().muted))
            .hover(|s| s.bg(cx.theme().muted))
            .child(SharedString::from(label))
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
    }

    fn render_field(
        &self,
        label: &str,
        input: &Entity<InputState>,
        hint: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(caption(label, cx))
            .child(div().w(px(260.)).child(Input::new(input)))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(hint.to_string())),
            )
    }
}

fn realtime_providers() -> Vec<(String, String)> {
    let mut providers: Vec<(String, String)> =
        llm::provider_config::ConfigurationSystem::load_providers_config(None)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, provider)| supports_realtime(provider))
            .map(|(id, provider)| (id, provider.label))
            .collect();
    providers.sort_by(|a, b| a.1.cmp(&b.1));
    providers
}

fn caption(text: &str, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(cx.theme().muted_foreground)
        .child(SharedString::from(text.to_string()))
}

impl Focusable for VoiceSection {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for VoiceSection {
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let provider_choices: Vec<_> = self
            .providers
            .iter()
            .map(|(id, label)| {
                let selected = self.config.provider == *id;
                let provider = id.clone();
                self.render_choice(
                    format!("voice-provider-{id}"),
                    label.clone(),
                    selected,
                    move |this, cx| {
                        let provider = provider.clone();
                        this.update(|config| config.provider = provider, cx)
                    },
                    cx,
                )
                .into_any_element()
            })
            .collect();
        let eagerness_choices: Vec<_> = EAGERNESS
            .iter()
            .map(|value| {
                let selected = self.config.vad_eagerness == *value;
                self.render_choice(
                    format!("voice-eagerness-{value}"),
                    value.to_string(),
                    selected,
                    move |this, cx| {
                        this.update(|config| config.vad_eagerness = value.to_string(), cx)
                    },
                    cx,
                )
                .into_any_element()
            })
            .collect();
        let touched_only = self.config.notify == NotifyScope::Touched;

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
                    .child("VOICE"),
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
                        "Voice mode is a hands-free assistant for all your conversations: ask \
                         what is running, start a conversation, or send one a message. Start it \
                         with the microphone in the title bar. It tells you when a conversation \
                         finishes, but never talks over you.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(caption("Realtime provider", cx))
                    .child(if provider_choices.is_empty() {
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "No provider with a realtime API yet. Add an OpenAI provider with \
                                 an API key (a ChatGPT subscription has no realtime access) or \
                                 an AI Core provider under Providers.",
                            )
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .children(provider_choices)
                            .into_any_element()
                    }),
            )
            .child(self.render_field(
                "Model",
                &self.model_input,
                "A speech-to-speech model of the provider. For AI Core: the name in the \
                 provider's models map whose deployment to use.",
                cx,
            ))
            .child(self.render_field(
                "Voice",
                &self.voice_input,
                "The model's voice, e.g. marin, cedar, alloy.",
                cx,
            ))
            .child(self.render_field(
                "Transcription model",
                &self.transcription_input,
                "Transcribes your speech for the transcript; empty turns it off.",
                cx,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(caption("Turn detection", cx))
                    .child(div().flex().gap_2().children(eagerness_choices))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "How soon the model answers after you pause. Low waits \
                                 longer, high answers sooner.",
                            ),
                    ),
            )
            .child(self.render_field(
                "Quiet time before notifications (ms)",
                &self.cooling_input,
                "How long it must be silent after the model spoke before it tells you about \
                 finished conversations.",
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Switch::new("voice-notify-touched")
                            .checked(touched_only)
                            .with_size(Size::Small)
                            .on_click({
                                let view = cx.entity();
                                move |enabled, _window, app| {
                                    let scope = if *enabled {
                                        NotifyScope::Touched
                                    } else {
                                        NotifyScope::All
                                    };
                                    view.update(app, |this, cx| {
                                        this.update(|config| config.notify = scope, cx)
                                    });
                                }
                            }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child("Only report conversations started or messaged by voice"),
                    ),
            )
    }
}
