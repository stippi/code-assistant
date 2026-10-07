//! Building blocks for the crate's GPUI tests.

use crate::stores::Stores;
use code_assistant_core::mocks::MockDraftStore;
use std::sync::Arc;

/// In-memory stores; the test keeps this handle to inspect them or make them
/// fail, while [`Gpui`](crate::Gpui) writes through [`MockStores::stores`].
#[derive(Default, Clone)]
pub struct MockStores {
    pub drafts: MockDraftStore,
}

impl MockStores {
    pub fn stores(&self) -> Stores {
        Stores {
            drafts: Arc::new(self.drafts.clone()),
        }
    }
}
