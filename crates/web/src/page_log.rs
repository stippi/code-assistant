//! What a tab logged: console messages and network requests, collected in the
//! background so the model can ask after the fact why something failed.

use anyhow::Result;
use chromiumoxide::cdp::browser_protocol::log::{EnableParams as LogEnableParams, EventEntryAdded};
use chromiumoxide::cdp::browser_protocol::network::{
    EventLoadingFailed, EventRequestWillBeSent, EventResponseReceived,
};
use chromiumoxide::cdp::js_protocol::runtime::{
    EventConsoleApiCalled, EventExceptionThrown, RemoteObject,
};
use chromiumoxide::page::Page;
use futures::StreamExt;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

/// How many messages / requests a tab keeps; older ones are dropped.
const CAPACITY: usize = 500;

#[derive(Debug, Clone)]
pub struct ConsoleMessage {
    /// `log`, `info`, `warning`, `error`, `debug`, … or `exception`.
    pub level: String,
    pub text: String,
    /// `url:line` where it came from, when known.
    pub source: Option<String>,
}

impl ConsoleMessage {
    pub fn is_error(&self) -> bool {
        matches!(self.level.as_str(), "error" | "exception" | "assert")
    }

    pub fn line(&self) -> String {
        match &self.source {
            Some(source) => format!("[{}] {} ({source})", self.level, self.text),
            None => format!("[{}] {}", self.level, self.text),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NetworkRequest {
    pub id: String,
    pub method: String,
    pub url: String,
    pub resource_type: String,
    pub status: Option<i64>,
    pub mime_type: Option<String>,
    pub failure: Option<String>,
}

impl NetworkRequest {
    pub fn line(&self) -> String {
        let outcome = match (self.status, &self.failure) {
            (Some(status), Some(failure)) => format!("{status} ({failure})"),
            (Some(status), None) => status.to_string(),
            (None, Some(failure)) => format!("failed ({failure})"),
            (None, None) => "pending".to_string(),
        };
        let mime = self
            .mime_type
            .as_deref()
            .filter(|m| !m.is_empty())
            .map(|m| format!(" ({m})"))
            .unwrap_or_default();
        format!(
            "[{}] {} {} {} {}{mime}",
            self.id, self.method, outcome, self.resource_type, self.url
        )
    }
}

/// The console and network log of one tab, filled by background listeners.
#[derive(Default)]
pub(crate) struct PageLog {
    pub console: Mutex<VecDeque<ConsoleMessage>>,
    pub network: Mutex<VecDeque<NetworkRequest>>,
}

impl PageLog {
    fn push_console(&self, message: ConsoleMessage) {
        let mut console = self.console.lock().unwrap();
        if console.len() == CAPACITY {
            console.pop_front();
        }
        console.push_back(message);
    }

    fn update_request(&self, id: &str, f: impl FnOnce(&mut NetworkRequest)) {
        let mut network = self.network.lock().unwrap();
        if let Some(request) = network.iter_mut().rev().find(|r| r.id == id) {
            f(request);
        }
    }
}

/// Start the listeners that fill `log` from `page`'s events.
pub(crate) async fn spawn_listeners(page: &Page, log: Arc<PageLog>) -> Result<Vec<JoinHandle<()>>> {
    // Browser-side messages (failed resource loads, CSP and mixed-content
    // errors) only arrive with the Log domain on.
    page.execute(LogEnableParams::default()).await?;

    let mut tasks = Vec::new();

    let mut console = page.event_listener::<EventConsoleApiCalled>().await?;
    let sink = log.clone();
    tasks.push(tokio::spawn(async move {
        while let Some(event) = console.next().await {
            let text = event
                .args
                .iter()
                .map(remote_object_text)
                .collect::<Vec<_>>()
                .join(" ");
            let source = event
                .stack_trace
                .as_ref()
                .and_then(|st| st.call_frames.first())
                .map(|f| format!("{}:{}", f.url, f.line_number + 1));
            sink.push_console(ConsoleMessage {
                level: event.r#type.as_ref().to_string(),
                text,
                source,
            });
        }
    }));

    let mut exceptions = page.event_listener::<EventExceptionThrown>().await?;
    let sink = log.clone();
    tasks.push(tokio::spawn(async move {
        while let Some(event) = exceptions.next().await {
            let details = &event.exception_details;
            let text = details
                .exception
                .as_ref()
                .and_then(|e| e.description.clone())
                .unwrap_or_else(|| details.text.clone());
            let source = details
                .url
                .as_ref()
                .map(|url| format!("{url}:{}", details.line_number + 1));
            sink.push_console(ConsoleMessage {
                level: "exception".to_string(),
                text,
                source,
            });
        }
    }));

    let mut entries = page.event_listener::<EventEntryAdded>().await?;
    let sink = log.clone();
    tasks.push(tokio::spawn(async move {
        while let Some(event) = entries.next().await {
            let entry = &event.entry;
            let source = entry.url.as_ref().map(|url| match entry.line_number {
                Some(line) => format!("{url}:{}", line + 1),
                None => url.clone(),
            });
            sink.push_console(ConsoleMessage {
                level: entry.level.as_ref().to_string(),
                text: entry.text.clone(),
                source,
            });
        }
    }));

    let mut requests = page.event_listener::<EventRequestWillBeSent>().await?;
    let sink = log.clone();
    tasks.push(tokio::spawn(async move {
        while let Some(event) = requests.next().await {
            let id = event.request_id.inner().clone();
            let mut network = sink.network.lock().unwrap();
            if network.len() == CAPACITY {
                network.pop_front();
            }
            network.push_back(NetworkRequest {
                id,
                method: event.request.method.clone(),
                url: event.request.url.clone(),
                resource_type: event
                    .r#type
                    .as_ref()
                    .map(|t| t.as_ref().to_lowercase())
                    .unwrap_or_else(|| "other".to_string()),
                status: None,
                mime_type: None,
                failure: None,
            });
        }
    }));

    let mut responses = page.event_listener::<EventResponseReceived>().await?;
    let sink = log.clone();
    tasks.push(tokio::spawn(async move {
        while let Some(event) = responses.next().await {
            sink.update_request(event.request_id.inner(), |r| {
                r.status = Some(event.response.status);
                r.mime_type = Some(event.response.mime_type.clone());
            });
        }
    }));

    let mut failures = page.event_listener::<EventLoadingFailed>().await?;
    let sink = log;
    tasks.push(tokio::spawn(async move {
        while let Some(event) = failures.next().await {
            let failure = if event.canceled == Some(true) {
                "canceled".to_string()
            } else {
                event.error_text.clone()
            };
            sink.update_request(event.request_id.inner(), |r| r.failure = Some(failure));
        }
    }));

    Ok(tasks)
}

/// A console argument as text: strings as-is, other values as JSON, objects
/// by their description.
fn remote_object_text(object: &RemoteObject) -> String {
    match &object.value {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(value) => value.to_string(),
        None => object
            .description
            .clone()
            .or_else(|| {
                object
                    .unserializable_value
                    .as_ref()
                    .map(|v| v.inner().clone())
            })
            .unwrap_or_else(|| object.r#type.as_ref().to_string()),
    }
}
