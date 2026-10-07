//! Building blocks for the crate's GPUI tests.

mod core;
mod main_screen;

pub use self::core::TestCore;
pub use main_screen::MainScreenTest;

use crate::shared::ui_state::{UiSessionState, UiStatePersistence};
use crate::stores::Stores;
use anyhow::Result;
use code_assistant_core::mocks::MockDraftStore;
use std::collections::HashMap;
use std::io::ErrorKind;
use std::sync::{Arc, Mutex};

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
