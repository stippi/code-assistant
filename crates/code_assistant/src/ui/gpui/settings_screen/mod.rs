//! Settings screen — full-screen view for configuring providers, models, and general preferences.

mod general_section;
mod models_section;
mod providers_section;

use gpui::{
    div, prelude::*, px, App, ClickEvent, Context, Entity, FocusHandle, Focusable, SharedString,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{ActiveTheme, Icon, Sizable, Size};
use tracing::debug;

/// Which section is currently displayed in the settings screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsSection {
    General,
    Providers,
    Models,
}

/// Events emitted by the settings screen.
#[derive(Clone, Debug)]
pub enum SettingsScreenEvent {
    /// User wants to go back to the main chat screen.
    Close,
}

impl gpui::EventEmitter<SettingsScreenEvent> for SettingsScreen {}

pub struct SettingsScreen {
    focus_handle: FocusHandle,
    active_section: SettingsSection,
    providers_section: Entity<providers_section::ProvidersSection>,
    models_section: Entity<models_section::ModelsSection>,
    general_section: Entity<general_section::GeneralSection>,
}

impl SettingsScreen {
    pub fn new(window: &mut gpui::Window, cx: &mut Context<Self>) -> Self {
        let providers_section = cx.new(|cx| providers_section::ProvidersSection::new(window, cx));
        let models_section = cx.new(|cx| models_section::ModelsSection::new(window, cx));
        let general_section = cx.new(|cx| general_section::GeneralSection::new(window, cx));

        Self {
            focus_handle: cx.focus_handle(),
            active_section: SettingsSection::Providers,
            providers_section,
            models_section,
            general_section,
        }
    }

    /// Switch to a specific section (e.g. auto-open Providers on first launch).
    #[allow(dead_code)]
    pub fn set_active_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        self.active_section = section;
        cx.notify();
    }

    fn on_back_clicked(
        &mut self,
        _: &ClickEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        cx.emit(SettingsScreenEvent::Close);
    }

    fn on_section_clicked(
        &mut self,
        section: SettingsSection,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.active_section = section;
        debug!("Settings: switched to {:?}", section);
        cx.notify();
    }

    fn render_nav_item(
        &self,
        section: SettingsSection,
        label: &str,
        icon_path: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active = self.active_section == section;
        div()
            .id(SharedString::from(format!("nav-{:?}", section)))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .when(is_active, |s| s.bg(cx.theme().muted))
            .hover(|s| s.bg(cx.theme().muted))
            .child(
                Icon::default()
                    .path(SharedString::from(icon_path.to_string()))
                    .with_size(Size::Small)
                    .text_color(if is_active {
                        cx.theme().foreground
                    } else {
                        cx.theme().muted_foreground
                    }),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(if is_active {
                        cx.theme().foreground
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(SharedString::from(label.to_string())),
            )
            .on_click(cx.listener(move |this, _event, window, cx| {
                this.on_section_clicked(section, window, cx);
            }))
    }
}

impl Focusable for SettingsScreen {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsScreen {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle(cx))
            .size_full()
            .flex()
            .flex_row()
            .bg(cx.theme().background)
            // Left navigation sidebar
            .child(
                div()
                    .flex_none()
                    .w(px(220.))
                    .h_full()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .flex()
                    .flex_col()
                    .justify_between()
                    .child(
                        // Nav items
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .p_3()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().muted_foreground)
                                    .mb_2()
                                    .child("Settings"),
                            )
                            .child(self.render_nav_item(
                                SettingsSection::General,
                                "General",
                                "icons/settings_alt.svg",
                                cx,
                            ))
                            .child(self.render_nav_item(
                                SettingsSection::Providers,
                                "Providers",
                                "icons/ai_anthropic.svg",
                                cx,
                            ))
                            .child(self.render_nav_item(
                                SettingsSection::Models,
                                "Models",
                                "icons/brain.svg",
                                cx,
                            )),
                    )
                    // Back button at bottom
                    .child(
                        div().p_3().child(
                            div()
                                .id("settings-back-btn")
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .cursor_pointer()
                                .hover(|s| s.bg(cx.theme().muted))
                                .child(
                                    Icon::default()
                                        .path(SharedString::from("icons/arrow_left.svg"))
                                        .with_size(Size::Small)
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Back"),
                                )
                                .on_click(cx.listener(Self::on_back_clicked)),
                        ),
                    ),
            )
            // Right content area (scrollable)
            .child(div().flex_1().h_full().overflow_y_scrollbar().p_6().child(
                match self.active_section {
                    SettingsSection::General => self.general_section.clone().into_any_element(),
                    SettingsSection::Providers => self.providers_section.clone().into_any_element(),
                    SettingsSection::Models => self.models_section.clone().into_any_element(),
                },
            ))
    }
}
