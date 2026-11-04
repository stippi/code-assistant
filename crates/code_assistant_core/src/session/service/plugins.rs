//! Plugin servers through the service, for a frontend's file renderers
//! (see [`crate::plugin_host::server`]).

use super::*;
use crate::plugin_host::{ServerLease, ServerTarget};

impl SessionService {
    /// Make sure the server a plugin's file renderer needs answers, starting
    /// it when nothing does. Hold the lease while the renderer shows a file;
    /// the server stops a while after the last lease is dropped.
    pub async fn start_plugin_server(&self, target: ServerTarget) -> Result<ServerLease> {
        self.call_io(move |_ctx| async move { crate::plugin_host::server::ensure(&target).await })
            .await
    }
}
