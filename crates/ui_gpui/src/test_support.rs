//! Building blocks for the crate's GPUI tests.

use crate::Gpui;
use crate::main_screen::MainScreen;
use crate::shared::ui_state::{UiSessionState, UiStatePersistence};
use crate::stores::Stores;
use anyhow::Result;
use code_assistant_core::mocks::MockDraftStore;
use code_assistant_core::session::{EventPayload, SessionEvent};
use code_assistant_core::ui::UiEvent;
use gpui_kit::component::Root;
use gpui_kit::{AppContext, Entity, TestAppContext, VisualTestContext};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

/// The main screen in a test window, wired to a [`Gpui`] like in the app but
/// with in-memory stores and without a session service: the test plays the
/// core by feeding events in, and inspects what the screen and the stores
/// show.
pub struct MainScreenTest {
    pub cx: VisualTestContext,
    pub gpui: Gpui,
    pub stores: MockStores,
    pub main_screen: Entity<MainScreen>,
}

impl MainScreenTest {
    pub fn new(cx: &mut TestAppContext) -> Self {
        let stores = MockStores::default();
        let gpui = Gpui::new(stores.stores());
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
        *self.gpui.project_sidebar.lock().unwrap() = None;
        *self.gpui.event_task.lock().unwrap() = None;
    }
}

/// In-memory stores; the test keeps this handle to inspect them or make them
/// fail, while [`Gpui`](crate::Gpui) writes through [`MockStores::stores`].
#[derive(Default, Clone)]
pub struct MockStores {
    pub drafts: MockDraftStore,
    pub ui_state: MockUiStatePersistence,
}

impl MockStores {
    pub fn stores(&self) -> Stores {
        Stores {
            drafts: Arc::new(self.drafts.clone()),
            ui_state: Arc::new(self.ui_state.clone()),
        }
    }
}

/// In-memory [`UiStatePersistence`] that can be told to fail.
#[derive(Default, Clone)]
pub struct MockUiStatePersistence {
    states: Arc<Mutex<HashMap<String, UiSessionState>>>,
    read_error: Arc<Mutex<Option<ErrorKind>>>,
    write_error: Arc<Mutex<Option<ErrorKind>>>,
}

impl MockUiStatePersistence {
    /// Make every following `load` fail with `kind`, or succeed again (`None`).
    pub fn fail_reads(&self, kind: Option<ErrorKind>) {
        *self.read_error.lock().unwrap() = kind;
    }

    /// Make every following `save` and `delete` fail with `kind`, or succeed
    /// again (`None`).
    pub fn fail_writes(&self, kind: Option<ErrorKind>) {
        *self.write_error.lock().unwrap() = kind;
    }

    /// The stored state of a session, bypassing any injected failure.
    pub fn stored(&self, session_id: &str) -> Option<UiSessionState> {
        self.states.lock().unwrap().get(session_id).cloned()
    }

    fn check(error: &Mutex<Option<ErrorKind>>) -> Result<()> {
        match *error.lock().unwrap() {
            Some(kind) => Err(std::io::Error::from(kind).into()),
            None => Ok(()),
        }
    }
}

impl UiStatePersistence for MockUiStatePersistence {
    fn load(&self, session_id: &str) -> Result<Option<UiSessionState>> {
        Self::check(&self.read_error)?;
        Ok(self.stored(session_id))
    }

    fn save(&self, session_id: &str, state: &UiSessionState) -> Result<()> {
        Self::check(&self.write_error)?;
        self.states
            .lock()
            .unwrap()
            .insert(session_id.to_string(), state.clone());
        Ok(())
    }

    fn delete(&self, session_id: &str) -> Result<()> {
        Self::check(&self.write_error)?;
        self.states.lock().unwrap().remove(session_id);
        Ok(())
    }
}
