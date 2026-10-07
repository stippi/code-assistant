//! [`MainScreenTest`]: the main screen in a test window.

use super::{MockStores, TestCore};
use crate::Gpui;
use crate::main_screen::MainScreen;
use code_assistant_core::session::{EventPayload, SessionEvent};
use code_assistant_core::ui::UiEvent;
use gpui_kit::component::Root;
use gpui_kit::{AppContext, Entity, TestAppContext, VisualTestContext};
use std::cell::RefCell;
use std::rc::Rc;

/// The main screen in a test window, wired to a [`Gpui`] like in the app but
/// with in-memory stores. Without a core ([`MainScreenTest::new`]) the test
/// plays the core by feeding events in; with one
/// ([`MainScreenTest::with_core`]) the screen's commands reach a real session
/// service.
pub struct MainScreenTest {
    pub cx: VisualTestContext,
    pub gpui: Gpui,
    pub stores: MockStores,
    pub main_screen: Entity<MainScreen>,
    pub core: Option<TestCore>,
}

impl MainScreenTest {
    pub fn new(cx: &mut TestAppContext) -> Self {
        Self::build(cx, None)
    }

    /// The screen in front of a real session core. Its worker runs on other
    /// threads, so waiting for it takes [`MainScreenTest::wait_until`].
    pub fn with_core(cx: &mut TestAppContext, core: TestCore) -> Self {
        cx.executor().allow_parking();
        Self::build(cx, Some(core))
    }

    fn build(cx: &mut TestAppContext, core: Option<TestCore>) -> Self {
        let stores = MockStores::default();
        let gpui = Gpui::new(stores.stores());
        if let Some(core) = &core {
            gpui.set_session_service(core.service.clone());
        }
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::shared::file_icons::init(cx);
            crate::init(cx);
            gpui.install(cx);
        });

        let main_screen = Rc::new(RefCell::new(None));
        let window = cx.add_window(|window, cx| {
            let (messages_view, sidebar) = gpui.new_session_views(window, cx);
            let screen = cx.new(|cx| MainScreen::new(messages_view, sidebar, window, cx));
            *main_screen.borrow_mut() = Some(screen.clone());
            Root::new(screen, window, cx)
        });
        let main_screen = main_screen
            .take()
            .expect("main screen built with the window");

        let mut test = Self {
            cx: VisualTestContext::from_window(window.into(), cx),
            gpui,
            stores,
            main_screen,
            core,
        };
        test.focus_input();
        test.settle();
        test
    }

    /// Run everything that is due, including the event loop's pauses
    /// between batches and debounced writes, then render.
    pub fn settle(&mut self) {
        self.cx.run_until_parked();
        self.cx
            .executor()
            .advance_clock(crate::shared::ui_state::debounce_duration());
        self.cx.run_until_parked();
        self.cx.update(|window, _| window.refresh());
        self.cx.run_until_parked();
    }

    /// Settle until `condition` holds, for work done on the core's threads.
    /// Fails after a generous timeout naming `what` was awaited.
    pub fn wait_until(&mut self, what: &str, mut condition: impl FnMut(&mut Self) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            self.settle();
            if condition(self) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting until {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn core(&self) -> &TestCore {
        self.core.as_ref().expect("a test with a core")
    }

    /// A new session in the core, selected like a click in the sidebar does.
    pub fn open_new_session(&mut self) -> String {
        let session_id = self.core().create_session();
        self.gpui.cmd_load_session(session_id.clone(), None);
        let id = session_id.clone();
        self.wait_until("the new session is shown", |test| {
            test.gpui.get_current_session_id().as_deref() == Some(id.as_str())
        });
        session_id
    }

    /// Type a message and press Enter.
    pub fn send(&mut self, text: &str) {
        self.type_text(text);
        self.cx.simulate_keystrokes("enter");
        self.settle();
    }

    /// The shown conversation, one `"<role>: <text>"` line per message that
    /// shows anything.
    pub fn transcript(&mut self) -> Vec<String> {
        let messages = self.gpui.message_queue.lock().unwrap().clone();
        self.cx.update(|_, cx| {
            messages
                .iter()
                .map(|message| message.read(cx))
                .filter(|message| !message.is_empty())
                .map(|message| message.transcript_line(cx))
                .collect()
        })
    }

    /// The message shown as waiting for the running agent.
    pub fn pending_message(&mut self) -> Option<String> {
        let view = self.gpui.messages_view.lock().unwrap().clone()?;
        view.read_with(&self.cx, |view, _| view.pending_message())
    }

    /// Click the element rendered with `debug_selector(selector)`.
    pub fn click(&mut self, selector: &'static str) {
        let bounds = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("nothing rendered as {selector}"));
        self.cx
            .simulate_click(bounds.center(), gpui_kit::Modifiers::none());
        self.settle();
    }

    /// Whether an element rendered with `debug_selector(selector)` is shown.
    pub fn shows(&mut self, selector: &'static str) -> bool {
        self.cx.debug_bounds(selector).is_some()
    }

    /// Whether the viewed session's agent is working.
    pub fn agent_is_running(&mut self) -> bool {
        self.gpui
            .current_session_activity_state
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|state| {
                *state != code_assistant_core::session::instance::SessionActivityState::Idle
            })
    }

    /// Show a session, as selecting it in the sidebar ends up doing once the
    /// core has loaded it.
    pub fn view_session(&mut self, session_id: &str) {
        *self.gpui.current_session_id.lock().unwrap() = Some(session_id.to_string());
        self.settle();
    }

    /// An event the core publishes for `session_id`, delivered through the
    /// event bridge.
    pub fn receive(&mut self, session_id: &str, event: UiEvent) {
        let event = SessionEvent {
            session_id: Some(session_id.to_string()),
            payload: EventPayload::Ui(event),
        };
        futures::executor::block_on(self.gpui.handle_stream_event(event));
        self.settle();
    }

    /// The result of a command arriving in the UI event queue, as the
    /// session service's answer would.
    pub fn push(&mut self, event: UiEvent) {
        self.gpui.push_event(event);
        self.settle();
    }

    pub fn focus_input(&mut self) {
        let input = self
            .main_screen
            .read_with(&self.cx, |screen, _| screen.input_area().clone());
        input.update_in(&mut self.cx, |input, window, cx| {
            input.focus_text(window, cx)
        });
    }

    /// Type into the composer, keystroke by keystroke as far as the input is
    /// concerned.
    pub fn type_text(&mut self, text: &str) {
        self.focus_input();
        self.cx.simulate_input(text);
        self.settle();
    }

    /// The composer's text.
    pub fn input_text(&mut self) -> String {
        self.main_screen.read_with(&self.cx, |screen, cx| {
            screen.input_area().read(cx).get_content(cx).0
        })
    }

    /// Whether the composer shows the banner of a message edit.
    pub fn is_editing(&mut self) -> bool {
        self.main_screen.read_with(&self.cx, |screen, cx| {
            screen.input_area().read(cx).is_editing()
        })
    }
}

impl Drop for MainScreenTest {
    fn drop(&mut self) {
        // `Gpui` holds view handles and the event loop, which (unlike in the
        // app) must not outlive the test's app.
        *self.gpui.messages_view.lock().unwrap() = None;
        self.gpui.message_queue.lock().unwrap().clear();
        *self.gpui.project_sidebar.lock().unwrap() = None;
        *self.gpui.event_task.lock().unwrap() = None;
    }
}
