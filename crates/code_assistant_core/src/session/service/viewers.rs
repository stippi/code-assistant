//! File viewers from plugins through the service, for a frontend's Files
//! view (see [`crate::plugin_host::viewer`]).

use super::*;
use crate::plugin_host::viewer::{self, ViewerRequest, WebViewer};

impl SessionService {
    /// Open a plugin's file viewer for a file: starts its server when
    /// needed and a browser tab for the page. The viewer stays open while
    /// its [`ViewerControl`](viewer::ViewerControl) lives.
    pub async fn open_file_viewer(&self, request: ViewerRequest) -> Result<WebViewer> {
        self.call_io(move |_ctx| viewer::open(request)).await
    }
}
