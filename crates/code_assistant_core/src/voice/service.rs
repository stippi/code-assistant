//! [`VoiceService`]: the frontends' handle to start, stop and mute voice
//! mode.

use super::agent::{Control, VoiceAgent};
use super::{AudioFactory, VoiceActivity, VoiceConfig, VoiceStatus};
use crate::session::SessionService;
use crate::session::event_stream::EventStream;
use crate::ui::UiEvent;
use anyhow::Result;
use llm::realtime::RealtimeConnector;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{error, info};

/// Builds the connector for a voice session from its configuration.
pub type ConnectorFactory =
    Arc<dyn Fn(&VoiceConfig) -> Result<Arc<dyn RealtimeConnector>> + Send + Sync>;

/// The connector for the configured provider (OpenAI or AI Core).
pub fn default_connector_factory() -> ConnectorFactory {
    Arc::new(|config: &VoiceConfig| config.connector())
}

#[derive(Debug)]
enum Command {
    Start,
    Stop,
    SetMuted(bool),
}

/// Cloneable handle; the voice agent runs in the worker future returned by
/// [`VoiceService::new`], which the application spawns on its backend
/// runtime. Status changes arrive as app-scoped
/// [`UiEvent::VoiceStatusChanged`] events.
#[derive(Clone)]
pub struct VoiceService {
    tx: mpsc::UnboundedSender<Command>,
}

impl VoiceService {
    pub fn new(
        sessions: SessionService,
        events: EventStream,
        audio: AudioFactory,
        connectors: ConnectorFactory,
    ) -> (Self, impl Future<Output = ()>) {
        Self::with_config_loader(
            sessions,
            events,
            audio,
            connectors,
            Arc::new(VoiceConfig::load),
        )
    }

    /// Like [`Self::new`], reading the configuration through `load_config`
    /// on every start (tests pass a fixed one).
    pub fn with_config_loader(
        sessions: SessionService,
        events: EventStream,
        audio: AudioFactory,
        connectors: ConnectorFactory,
        load_config: Arc<dyn Fn() -> VoiceConfig + Send + Sync>,
    ) -> (Self, impl Future<Output = ()>) {
        let (tx, mut rx) = mpsc::unbounded_channel::<Command>();
        let worker = async move {
            let mut running: Option<Running> = None;
            while let Some(command) = rx.recv().await {
                if running.as_ref().is_some_and(|r| r.task.is_finished()) {
                    running = None;
                }
                match command {
                    Command::Start => {
                        if running.is_some() {
                            continue;
                        }
                        running = start(load_config(), &sessions, &events, &audio, &connectors);
                    }
                    Command::Stop => {
                        if let Some(r) = running.take() {
                            let _ = r.control.send(Control::Stop);
                            let _ = r.task.await;
                        }
                        publish(&events, VoiceStatus::default());
                    }
                    Command::SetMuted(muted) => {
                        if let Some(r) = &running {
                            let _ = r.control.send(Control::SetMuted(muted));
                        }
                    }
                }
            }
            if let Some(r) = running {
                let _ = r.control.send(Control::Stop);
                let _ = r.task.await;
            }
        };
        (Self { tx }, worker)
    }

    pub fn start(&self) {
        let _ = self.tx.send(Command::Start);
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Command::Stop);
    }

    pub fn set_muted(&self, muted: bool) {
        let _ = self.tx.send(Command::SetMuted(muted));
    }
}

struct Running {
    control: mpsc::UnboundedSender<Control>,
    task: JoinHandle<()>,
}

fn start(
    config: VoiceConfig,
    sessions: &SessionService,
    events: &EventStream,
    audio: &AudioFactory,
    connectors: &ConnectorFactory,
) -> Option<Running> {
    publish(
        events,
        VoiceStatus {
            activity: VoiceActivity::Connecting,
            ..VoiceStatus::default()
        },
    );
    let connector = match connectors(&config) {
        Ok(connector) => connector,
        Err(e) => {
            fail(events, e);
            return None;
        }
    };
    let (control, control_rx) = mpsc::unbounded_channel();
    let sessions = sessions.clone();
    let events = events.clone();
    let audio = audio.clone();
    let task = tokio::spawn(async move {
        let agent =
            match VoiceAgent::start(config, connector, sessions, events.clone(), audio).await {
                Ok(agent) => agent,
                Err(e) => return fail(&events, e),
            };
        info!("Voice mode started");
        match agent.run(control_rx).await {
            Ok(()) => info!("Voice mode stopped"),
            Err(e) => fail(&events, e),
        }
    });
    Some(Running { control, task })
}

fn fail(events: &EventStream, error: anyhow::Error) {
    error!("Voice mode failed: {error:#}");
    publish(
        events,
        VoiceStatus {
            activity: VoiceActivity::Failed(format!("{error:#}")),
            ..VoiceStatus::default()
        },
    );
}

fn publish(events: &EventStream, status: VoiceStatus) {
    events.publish_app(UiEvent::VoiceStatusChanged { status });
}
