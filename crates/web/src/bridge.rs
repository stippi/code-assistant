//! A page's own scripts talking to the embedder.
//!
//! The browser tools look at pages from outside. A page the app hosts (a
//! file viewer, say) needs a channel of its own: a script that runs in every
//! document before the page's scripts ([`Tab::add_init_script`]), and a
//! function the page calls to hand the embedder a string ([`Tab::bind`]).
//! The way back is [`Tab::javascript`].

use crate::tab::Tab;
use anyhow::Result;
use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use chromiumoxide::cdp::js_protocol::runtime::{AddBindingParams, EventBindingCalled};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// What the page passes to a bound function, in call order. Dropping it
/// stops listening (the function stays defined).
pub struct Binding {
    calls: mpsc::UnboundedReceiver<String>,
    task: JoinHandle<()>,
}

impl Binding {
    /// The next payload, `None` once the tab is gone.
    pub async fn next(&mut self) -> Option<String> {
        self.calls.recv().await
    }
}

impl Drop for Binding {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Tab {
    /// Run `source` in every document the tab loads from now on, before the
    /// page's own scripts (in frames too; a script that is only for the top
    /// document checks `window.top === window`).
    pub async fn add_init_script(&self, source: &str) -> Result<()> {
        self.bounded("adding a page script", self.timeouts().command, async {
            self.page()
                .execute(AddScriptToEvaluateOnNewDocumentParams::new(source))
                .await?;
            Ok(())
        })
        .await
    }

    /// Define `window[name](payload: string)` in every document from now on;
    /// what the page passes arrives through the returned [`Binding`].
    pub async fn bind(&self, name: &str) -> Result<Binding> {
        let mut events = self.page().event_listener::<EventBindingCalled>().await?;
        self.bounded("adding a page binding", self.timeouts().command, async {
            self.page().execute(AddBindingParams::new(name)).await?;
            Ok(())
        })
        .await?;
        let (tx, calls) = mpsc::unbounded_channel();
        let name = name.to_string();
        let task = tokio::spawn(async move {
            while let Some(event) = events.next().await {
                if event.name == name && tx.send(event.payload.clone()).is_err() {
                    return;
                }
            }
        });
        Ok(Binding { calls, task })
    }
}
