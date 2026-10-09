//! Voice mode in the title bar: start/stop, mute, the voice agent's state,
//! and a popover with the voice conversation's transcript.

use super::MainScreen;
use crate::Gpui;
use code_assistant_core::voice::{TranscriptRole, VoiceActivity};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size};
use gpui_kit::{AnyElement, Context, SharedString, div, prelude::*, px};

impl MainScreen {
    /// The title-bar controls, or `None` when the build has no voice mode.
    pub(super) fn render_voice_controls(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let gpui = cx.try_global::<Gpui>()?;
        if !gpui.voice_available() {
            return None;
        }
        let status = gpui.voice_status();
        let active = status.activity.is_active();
        let failure = match &status.activity {
            VoiceActivity::Failed(message) => Some(SharedString::from(message.clone())),
            _ => None,
        };
        let failed = failure.is_some();
        // Without a voice model there is nothing to start; a running voice
        // session stays controllable either way.
        if !gpui.voice_configured() && !active && !failed {
            return None;
        }

        let mic_color = if failed {
            cx.theme().danger
        } else if active {
            cx.theme().primary
        } else {
            cx.theme().muted_foreground
        };
        let mic_tooltip = if active {
            "Stop voice mode"
        } else {
            "Start voice mode"
        };
        let mic = div()
            .id("voice-toggle-btn")
            .size(px(28.))
            .rounded_sm()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(cx.theme().muted))
            .when(active, |el| el.bg(cx.theme().muted))
            .child(
                Icon::default()
                    .path(SharedString::from("icons/mic.svg"))
                    .with_size(Size::Small)
                    .text_color(mic_color),
            )
            .tooltip(move |window, cx| Tooltip::new(mic_tooltip).build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(gpui) = cx.try_global::<Gpui>() {
                    gpui.toggle_voice();
                }
                this.voice_transcript_open = false;
                cx.notify();
            }));

        let mut row = div().flex().items_center().gap_1().mr_1();
        if let Some(message) = failure {
            row = row.child(
                div()
                    .id("voice-failure-chip")
                    .h(px(24.))
                    .px_2()
                    .rounded_md()
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child("Voice stopped")
                    .tooltip(move |window, cx| Tooltip::new(message.clone()).build(window, cx)),
            );
        }
        if active {
            let muted = status.muted;
            let label = match status.activity {
                VoiceActivity::Connecting => "Connecting…",
                VoiceActivity::UserSpeaking => "Hearing you",
                VoiceActivity::Speaking => "Speaking",
                _ if muted => "Muted",
                _ => "Listening",
            };
            let label = if status.queued_notifications > 0 {
                format!("{label} · {} waiting", status.queued_notifications)
            } else {
                label.to_string()
            };
            let speaking = status.activity == VoiceActivity::Speaking;
            row = row
                .child(
                    div()
                        .id("voice-status-chip")
                        .h(px(24.))
                        .px_2()
                        .rounded_md()
                        .flex()
                        .items_center()
                        .gap_1()
                        .cursor_pointer()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .hover(|s| s.bg(cx.theme().muted))
                        .when(self.voice_transcript_open, |el| el.bg(cx.theme().muted))
                        .child(
                            Icon::default()
                                .path(SharedString::from("icons/audio_lines.svg"))
                                .with_size(Size::XSmall)
                                .text_color(if speaking {
                                    cx.theme().primary
                                } else {
                                    cx.theme().muted_foreground
                                }),
                        )
                        .child(SharedString::from(label))
                        .tooltip(|window, cx| Tooltip::new("Show the transcript").build(window, cx))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.voice_transcript_open = !this.voice_transcript_open;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .id("voice-mute-btn")
                        .size(px(28.))
                        .rounded_sm()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(cx.theme().muted))
                        .child(
                            Icon::default()
                                .path(SharedString::from(if muted {
                                    "icons/mic_off.svg"
                                } else {
                                    "icons/mic.svg"
                                }))
                                .with_size(Size::Small)
                                .text_color(if muted {
                                    cx.theme().danger
                                } else {
                                    cx.theme().muted_foreground
                                }),
                        )
                        .tooltip(move |window, cx| {
                            Tooltip::new(if muted { "Unmute" } else { "Mute" }).build(window, cx)
                        })
                        .on_click(move |_, _, cx| {
                            if let Some(gpui) = cx.try_global::<Gpui>() {
                                gpui.set_voice_muted(!muted);
                            }
                        }),
                );
        }
        Some(row.child(mic).into_any_element())
    }

    /// The transcript popover below the title bar, while open.
    pub(super) fn render_voice_transcript(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.voice_transcript_open {
            return None;
        }
        let gpui = cx.try_global::<Gpui>()?;
        if !gpui.voice_status().activity.is_active() {
            return None;
        }
        let transcript = gpui.voice_transcript();

        let entries = transcript.into_iter().map(|entry| {
            let (label, color) = match entry.role {
                TranscriptRole::User => ("You", cx.theme().foreground),
                TranscriptRole::Assistant => ("Assistant", cx.theme().primary),
                TranscriptRole::Tool => ("Tool", cx.theme().muted_foreground),
                TranscriptRole::Notification => ("Notification", cx.theme().muted_foreground),
            };
            // A notification's first lines name the conversations; the
            // instructions to the model below them are noise here.
            let text = match entry.role {
                TranscriptRole::Notification => entry
                    .text
                    .lines()
                    .skip(1)
                    .take_while(|line| !line.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => entry.text,
            };
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_xs().text_color(color).child(label))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .child(SharedString::from(text)),
                )
        });

        let empty = div()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Say something — the transcript appears here.");

        Some(
            div()
                .absolute()
                .top(px(52.))
                .right(px(16.))
                .w(px(380.))
                .max_h(px(440.))
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .rounded_lg()
                .shadow_lg()
                .child(
                    div()
                        .id("voice-transcript")
                        .max_h(px(440.))
                        .overflow_y_scroll()
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .children(entries)
                        .when(gpui.voice_transcript().is_empty(), |el| el.child(empty)),
                )
                .into_any_element(),
        )
    }
}
