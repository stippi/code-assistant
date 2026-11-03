//! Self-update state mirrored from the core, and the commands to drive it.

use crate::Gpui;
use code_assistant_core::updates::{UpdateService, UpdateState};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct UpdatesState {
    service: Arc<Mutex<Option<UpdateService>>>,
    state: Arc<Mutex<Option<UpdateState>>>,
}

impl Gpui {
    /// Install the update service; without one the app shows no updates.
    pub fn set_update_service(&self, service: UpdateService) {
        *self.updates.state.lock().unwrap() = Some(service.state());
        *self.updates.service.lock().unwrap() = Some(service);
    }

    pub(crate) fn update_service(&self) -> Option<UpdateService> {
        self.updates.service.lock().unwrap().clone()
    }

    pub fn update_state(&self) -> Option<UpdateState> {
        self.updates.state.lock().unwrap().clone()
    }

    pub(crate) fn apply_update_state(&self, state: UpdateState) {
        *self.updates.state.lock().unwrap() = Some(state);
    }

    pub(crate) fn cmd_check_for_update(&self) {
        if let Some(service) = self.update_service() {
            self.dispatch(async move { service.check().await });
        }
    }

    pub(crate) fn cmd_download_update(&self) {
        if let Some(service) = self.update_service() {
            self.dispatch(async move { service.download().await });
        }
    }
}
