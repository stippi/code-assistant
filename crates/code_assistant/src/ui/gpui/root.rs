//! Root view (AppShell) — switches between MainScreen and SettingsScreen.
//!
//! When the app starts without a valid configuration (no providers/models),
//! it automatically shows the settings screen. Otherwise it shows the main
//! chat screen with its sidebar, messages, and input area.

use super::chat_sidebar::ChatSidebar;
use super::main_screen::{MainScreen, MainScreenEvent};
use super::messages::MessagesView;
use super::settings_screen::{SettingsScreen, SettingsScreenEvent};
use gpui::{div, prelude::*, App, Context, Entity, FocusHandle, Focusable, Subscription};
use tracing::debug;

/// Which top-level view is currently displayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveView {
    Main,
    Settings,
}

pub struct RootView {
    active_view: ActiveView,
    main_screen: Entity<MainScreen>,
    settings_screen: Option<Entity<SettingsScreen>>,
    focus_handle: FocusHandle,
    _main_screen_subscription: Subscription,
    _settings_screen_subscription: Option<Subscription>,
}

impl RootView {
    pub fn new(
        messages_view: Entity<MessagesView>,
        chat_sidebar: Entity<ChatSidebar>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Create the main screen (former RootView)
        let main_screen = cx.new(|cx| MainScreen::new(messages_view, chat_sidebar, window, cx));

        // Subscribe to main screen events
        let main_screen_subscription =
            cx.subscribe_in(&main_screen, window, Self::on_main_screen_event);

        // Check if we should auto-open settings (no valid config)
        let should_open_settings = Self::needs_initial_setup();

        let mut root = Self {
            active_view: if should_open_settings {
                ActiveView::Settings
            } else {
                ActiveView::Main
            },
            main_screen,
            settings_screen: None,
            focus_handle: cx.focus_handle(),
            _main_screen_subscription: main_screen_subscription,
            _settings_screen_subscription: None,
        };

        // Create settings screen if needed
        if should_open_settings {
            root.ensure_settings_screen(window, cx);
        }

        root
    }

    /// Returns `true` if no valid provider or model config exists.
    fn needs_initial_setup() -> bool {
        match llm::provider_config::ConfigurationSystem::load() {
            Ok(config) => config.models.is_empty(),
            Err(_) => true,
        }
    }

    /// Create or return the settings screen entity.
    fn ensure_settings_screen(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.settings_screen.is_none() {
            let settings = cx.new(|cx| SettingsScreen::new(window, cx));
            let subscription = cx.subscribe_in(&settings, window, Self::on_settings_screen_event);
            self.settings_screen = Some(settings);
            self._settings_screen_subscription = Some(subscription);
        }
    }

    fn on_main_screen_event(
        &mut self,
        _main_screen: &Entity<MainScreen>,
        event: &MainScreenEvent,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            MainScreenEvent::OpenSettings => {
                debug!("RootView: Opening settings screen");
                self.ensure_settings_screen(window, cx);
                self.active_view = ActiveView::Settings;
                cx.notify();
            }
        }
    }

    fn on_settings_screen_event(
        &mut self,
        _settings_screen: &Entity<SettingsScreen>,
        event: &SettingsScreenEvent,
        _window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SettingsScreenEvent::Close => {
                debug!("RootView: Closing settings screen");
                self.active_view = ActiveView::Main;
                cx.notify();
            }
        }
    }
}

impl Focusable for RootView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus_handle(cx))
            .child(match self.active_view {
                ActiveView::Main => self.main_screen.clone().into_any_element(),
                ActiveView::Settings => {
                    if let Some(ref settings) = self.settings_screen {
                        settings.clone().into_any_element()
                    } else {
                        // Shouldn't happen, but fallback
                        self.main_screen.clone().into_any_element()
                    }
                }
            })
    }
}
