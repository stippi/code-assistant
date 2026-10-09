//! The voice agent's event loop.
//!
//! One task per voice session. It owns the realtime connection, the audio
//! devices and the [`Floor`], and turns everything that happens — server
//! events, audio reports, tool completions, conversation events, timers —
//! into floor inputs, then carries out the floor's commands.

use super::floor::{Floor, FloorCommand, FloorConfig, FloorInput, FloorState, Timer};
use super::source::{NotificationSource, OpenRequests};
use super::tools::{ToolEffect, ToolOutcome, VoiceTools};
use super::{
    AudioEvent, AudioFactory, TranscriptEntry, TranscriptRole, VoiceActivity, VoiceAudio,
    VoiceConfig, VoiceStatus,
};
use crate::session::SessionService;
use crate::session::event_stream::{EventStream, StreamError, Subscription};
use crate::ui::UiEvent;
use anyhow::{Context, Result, anyhow};
use llm::realtime::{
    ClientEvent, Incoming, RealtimeConnection, RealtimeConnector, SAMPLE_RATE, ServerEvent,
    SessionSettings, decode_pcm16,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, warn};

/// Commands from the [`super::VoiceService`] to a running agent.
#[derive(Debug)]
pub(super) enum Control {
    Stop,
    SetMuted(bool),
}

const RECONNECT_ATTEMPTS: u32 = 3;
/// Bounds one connection attempt, including the credentials and endpoint
/// lookups a connector does first.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Bounds one voice tool call; all of them return without waiting for a
/// conversation's agent.
const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
/// Transcript entries replayed into a renewed realtime session.
const RESEED_ENTRIES: usize = 30;

const INSTRUCTIONS: &str = "\
You are the voice assistant of a coding-agent app. The user talks to you hands-free about their \
conversations with coding agents: what is running, what an agent found or changed, what an agent \
should do next. You cannot read or change code yourself. You act only through your tools: list \
conversations and projects, read a conversation, and send a message to a conversation (which starts \
a new one when you give no conversation id).

Speak briefly: one to three sentences unless the user asks for detail. Refer to conversations by \
title, never read ids or paths aloud, and summarise agent answers instead of reading code or long \
lists. When you start or message a conversation, confirm in a few words and move on; the agent \
works in the background. When you write a message for an agent, phrase it as a clear instruction \
with all the context the user gave you. If it is unclear which conversation the user means, ask.

Messages marked [background notification] come from the app, not the user. They tell you that a \
conversation finished or waits for the user. Mention them briefly when they are useful, after \
calling get_conversation for results. Answer in the language the user speaks.";

pub(super) struct VoiceAgent {
    config: VoiceConfig,
    connector: Arc<dyn RealtimeConnector>,
    events: EventStream,
    tools: VoiceTools,
    source: NotificationSource,
    subscription: Subscription,
    floor: Floor,
    connection: RealtimeConnection,
    audio: Box<dyn VoiceAudio>,
    audio_rx: mpsc::UnboundedReceiver<AudioEvent>,
    tool_tx: mpsc::UnboundedSender<(u64, String, ToolOutcome)>,
    tool_rx: mpsc::UnboundedReceiver<(u64, String, ToolOutcome)>,
    /// Bumped per realtime connection; tool results of an older one are
    /// dropped (their call ids mean nothing to the new session).
    generation: u64,
    timer: Option<(Timer, Instant)>,
    muted: bool,
    status: VoiceStatus,
    transcript: Vec<TranscriptEntry>,
    /// Samples handed to the speakers since the devices opened.
    queued_samples: u64,
    /// The assistant item whose audio plays, with the sample offset its
    /// audio starts at.
    audio_item: Option<(String, u64)>,
}

impl VoiceAgent {
    /// Open audio and the realtime session. Fails without side effects
    /// left running.
    pub(super) async fn start(
        config: VoiceConfig,
        connector: Arc<dyn RealtimeConnector>,
        service: SessionService,
        events: EventStream,
        audio_factory: AudioFactory,
    ) -> Result<Self> {
        let open_requests = OpenRequests::default();
        // Subscribe before reading the initial states, so no transition
        // falls in between.
        let subscription = service.subscribe();
        let source =
            NotificationSource::new(service.clone(), config.notify, open_requests.clone()).await?;
        let tools = VoiceTools::new(service, open_requests);

        let connection = connect(connector.as_ref()).await?;
        let (audio_tx, audio_rx) = mpsc::unbounded_channel();
        let audio = audio_factory(audio_tx).context("Failed to open the audio devices")?;
        let (tool_tx, tool_rx) = mpsc::unbounded_channel();
        let floor = Floor::new(FloorConfig {
            cooling: config.cooling(),
            ..FloorConfig::default()
        });

        let agent = Self {
            config,
            connector,
            events,
            tools,
            source,
            subscription,
            floor,
            connection,
            audio,
            audio_rx,
            tool_tx,
            tool_rx,
            generation: 0,
            timer: None,
            muted: false,
            status: VoiceStatus {
                activity: VoiceActivity::Connecting,
                ..VoiceStatus::default()
            },
            transcript: Vec::new(),
            queued_samples: 0,
            audio_item: None,
        };
        agent.send(ClientEvent::SessionUpdate(agent.session_settings()));
        Ok(agent)
    }

    /// Run until stopped. Returns the error that ended the session, if any.
    pub(super) async fn run(mut self, mut control: mpsc::UnboundedReceiver<Control>) -> Result<()> {
        self.publish_status();
        loop {
            let timer = self.timer;
            tokio::select! {
                command = control.recv() => match command {
                    Some(Control::SetMuted(muted)) => {
                        self.muted = muted;
                        self.audio.set_capture_muted(muted);
                        if muted {
                            self.floor_input(FloorInput::CaptureMuted);
                        }
                        self.publish_status();
                    }
                    Some(Control::Stop) | None => return Ok(()),
                },
                incoming = self.connection.incoming.recv() => match incoming {
                    Some(Incoming::Event(event)) => self.on_server_event(event),
                    Some(Incoming::Closed(reason)) => self.reconnect(reason).await?,
                    None => self.reconnect(None).await?,
                },
                audio = self.audio_rx.recv() => match audio {
                    Some(AudioEvent::Captured(samples)) => {
                        if !self.muted {
                            self.send(ClientEvent::AppendAudio(samples));
                        }
                    }
                    Some(AudioEvent::Drained) => self.floor_input(FloorInput::PlaybackDrained),
                    Some(AudioEvent::Failed(message)) => {
                        return Err(anyhow!("Audio device failed: {message}"));
                    }
                    None => return Err(anyhow!("The audio devices closed")),
                },
                Some((generation, call_id, outcome)) = self.tool_rx.recv() => {
                    if generation == self.generation {
                        self.on_tool_completed(call_id, outcome);
                    }
                }
                event = self.subscription.recv() => {
                    let inputs = match event {
                        Ok(event) => self.source.on_event(&event).await,
                        Err(StreamError::Lagged { .. }) => self.source.resync().await,
                        Err(StreamError::Closed) => return Err(anyhow!("The app is shutting down")),
                    };
                    for input in inputs {
                        self.floor_input(input);
                    }
                }
                _ = sleep_until(timer) => {
                    if let Some((fired, _)) = self.timer.take() {
                        self.floor_input(FloorInput::TimerFired(fired));
                    }
                }
            }
        }
    }

    fn on_server_event(&mut self, event: ServerEvent) {
        match event {
            ServerEvent::SessionCreated | ServerEvent::SessionUpdated => {
                if self.status.activity == VoiceActivity::Connecting {
                    self.status.activity = VoiceActivity::Listening;
                    self.publish_status();
                }
            }
            ServerEvent::ResponseCreated { .. } => self.floor_input(FloorInput::ResponseCreated),
            ServerEvent::OutputItemAdded { .. } => {}
            ServerEvent::AudioDelta { item_id, delta } => self.on_audio_delta(item_id, &delta),
            ServerEvent::AudioTranscriptDone { transcript, .. } => {
                self.record(TranscriptRole::Assistant, transcript)
            }
            ServerEvent::InputTranscription { transcript, .. } => {
                self.record(TranscriptRole::User, transcript)
            }
            ServerEvent::SpeechStarted => self.floor_input(FloorInput::SpeechStarted),
            ServerEvent::SpeechStopped => self.floor_input(FloorInput::SpeechStopped),
            ServerEvent::OutputItemDone { item } => {
                if item.kind == "function_call" {
                    let name = item.name.unwrap_or_default();
                    let call_id = item.call_id.unwrap_or_default();
                    let arguments = item.arguments.unwrap_or_default();
                    self.start_tool(name, call_id, arguments);
                }
            }
            ServerEvent::ResponseDone { response } => {
                debug!(
                    "Realtime response {} done: {:?}",
                    response.id, response.status
                );
                let pending = self.pending_playback();
                self.floor_input(FloorInput::ResponseDone {
                    pending_playback: pending,
                });
            }
            ServerEvent::Error { error } => {
                warn!(
                    "Realtime error ({}): {}",
                    error.code.as_deref().unwrap_or("-"),
                    error.message
                );
                self.floor_input(FloorInput::ServerError);
            }
            ServerEvent::Other => {}
        }
    }

    fn on_audio_delta(&mut self, item_id: String, delta: &str) {
        // Audio of a response the user interrupted, generated before the
        // server got our cancel (invariant 5).
        if !self.floor.plays_audio() {
            return;
        }
        let samples = match decode_pcm16(delta) {
            Ok(samples) => samples,
            Err(e) => {
                warn!("Undecodable realtime audio: {e}");
                return;
            }
        };
        if self
            .audio_item
            .as_ref()
            .is_none_or(|(id, _)| *id != item_id)
        {
            self.audio_item = Some((item_id, self.queued_samples));
        }
        self.queued_samples += samples.len() as u64;
        self.audio.play(samples);
    }

    fn start_tool(&mut self, name: String, call_id: String, arguments: String) {
        info!("Voice tool call {name} {arguments}");
        self.record(TranscriptRole::Tool, format!("{name} {arguments}"));
        self.floor_input(FloorInput::ToolStarted);
        let tools = self.tools.clone();
        let tx = self.tool_tx.clone();
        let generation = self.generation;
        tokio::spawn(async move {
            // Every call must report back: the floor holds notifications
            // while a tool runs.
            let call = tokio::spawn(async move { tools.call(&name, &arguments).await });
            let abort = call.abort_handle();
            let outcome = match tokio::time::timeout(TOOL_TIMEOUT, call).await {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(e)) => ToolOutcome::error(anyhow!("The tool failed: {e}")),
                Err(_) => {
                    abort.abort();
                    ToolOutcome::error(anyhow!(
                        "The tool did not finish within {}s",
                        TOOL_TIMEOUT.as_secs()
                    ))
                }
            };
            let _ = tx.send((generation, call_id, outcome));
        });
    }

    fn on_tool_completed(&mut self, call_id: String, outcome: ToolOutcome) {
        for effect in outcome.effects {
            match effect {
                ToolEffect::Read(conversation_id) => {
                    self.floor_input(FloorInput::ConversationRead { conversation_id })
                }
                ToolEffect::Touched(conversation_id) => self.source.touch(&conversation_id),
            }
        }
        self.floor_input(FloorInput::ToolCompleted {
            call_id,
            output: outcome.output,
        });
    }

    fn floor_input(&mut self, input: FloorInput) {
        let commands = self.floor.handle(input);
        self.execute(commands);
        self.sync_status();
    }

    fn execute(&mut self, commands: Vec<FloorCommand>) {
        for command in commands {
            match command {
                FloorCommand::CancelResponse => self.send(ClientEvent::CancelResponse),
                FloorCommand::StopPlayback => self.stop_playback(),
                FloorCommand::SendToolOutput { call_id, output } => {
                    self.send(ClientEvent::FunctionCallOutput { call_id, output })
                }
                FloorCommand::SendNotification(text) => {
                    self.record(TranscriptRole::Notification, text.clone());
                    self.send(ClientEvent::SystemMessage(text));
                }
                FloorCommand::CreateResponse => self.send(ClientEvent::CreateResponse),
                FloorCommand::StartTimer(timer, after) => {
                    self.timer = Some((timer, Instant::now() + after))
                }
                FloorCommand::CancelTimer => self.timer = None,
            }
        }
    }

    /// Barge-in: silence the speakers and cut the server's copy of the
    /// item to what the user heard, so the model does not believe it said
    /// the rest.
    fn stop_playback(&mut self) {
        let played = self.audio.played_samples();
        if let Some((item_id, start)) = self.audio_item.take() {
            if played < self.queued_samples {
                let heard = played.saturating_sub(start);
                self.send(ClientEvent::TruncateItem {
                    item_id,
                    audio_end_ms: samples_to_ms(heard),
                });
            }
        }
        self.audio.clear_playback();
        self.queued_samples = played;
    }

    fn pending_playback(&self) -> Duration {
        let pending = self
            .queued_samples
            .saturating_sub(self.audio.played_samples());
        Duration::from_millis(samples_to_ms(pending))
    }

    /// Replace a closed realtime session with a fresh one that knows the
    /// conversation so far.
    async fn reconnect(&mut self, reason: Option<String>) -> Result<()> {
        info!("Realtime session closed ({reason:?}); reconnecting");
        self.status.activity = VoiceActivity::Connecting;
        self.publish_status();
        self.audio.clear_playback();
        self.queued_samples = self.audio.played_samples();
        self.audio_item = None;

        let mut last_error = anyhow!("connection closed: {}", reason.unwrap_or_default());
        for attempt in 0..RECONNECT_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
            }
            match connect(self.connector.as_ref()).await {
                Ok(connection) => {
                    self.connection = connection;
                    self.generation += 1;
                    self.send(ClientEvent::SessionUpdate(self.session_settings()));
                    if let Some(summary) = self.transcript_summary() {
                        self.send(ClientEvent::SystemMessage(summary));
                    }
                    let commands = self.floor.reconnected();
                    self.execute(commands);
                    return Ok(());
                }
                Err(e) => {
                    warn!("Realtime reconnect attempt {} failed: {e:#}", attempt + 1);
                    last_error = e;
                }
            }
        }
        Err(last_error.context("The realtime session could not be renewed"))
    }

    fn transcript_summary(&self) -> Option<String> {
        let start = self.transcript.len().saturating_sub(RESEED_ENTRIES);
        let lines: Vec<String> = self.transcript[start..]
            .iter()
            .filter_map(|entry| match entry.role {
                TranscriptRole::User => Some(format!("User: {}", entry.text)),
                TranscriptRole::Assistant => Some(format!("You: {}", entry.text)),
                TranscriptRole::Tool => Some(format!("(tool call: {})", entry.text)),
                TranscriptRole::Notification => None,
            })
            .collect();
        (!lines.is_empty()).then(|| {
            format!(
                "The connection was renewed. The voice conversation so far:\n{}",
                lines.join("\n")
            )
        })
    }

    fn session_settings(&self) -> SessionSettings {
        SessionSettings {
            model: self
                .connector
                .declares_model()
                .then(|| self.config.model.clone()),
            instructions: INSTRUCTIONS.to_string(),
            voice: self.config.voice.clone(),
            tools: VoiceTools::definitions(),
            turn_detection: self.config.turn_detection(),
            transcription_model: self.config.transcription(),
        }
    }

    fn send(&self, event: ClientEvent) {
        // A closed connection shows up on the incoming side.
        let _ = self.connection.outgoing.send(event);
    }

    fn record(&mut self, role: TranscriptRole, text: String) {
        if text.trim().is_empty() {
            return;
        }
        let entry = TranscriptEntry { role, text };
        self.events.publish_app(UiEvent::VoiceTranscript {
            entry: entry.clone(),
        });
        self.transcript.push(entry);
    }

    fn sync_status(&mut self) {
        if self.status.activity == VoiceActivity::Connecting {
            return;
        }
        let activity = match self.floor.state() {
            FloorState::Speaking => VoiceActivity::Speaking,
            FloorState::UserTurn => VoiceActivity::UserSpeaking,
            FloorState::Idle | FloorState::Cooling => VoiceActivity::Listening,
        };
        let queued = self.floor.queued_notifications();
        if activity != self.status.activity || queued != self.status.queued_notifications {
            self.status.activity = activity;
            self.status.queued_notifications = queued;
            self.publish_status();
        }
    }

    fn publish_status(&mut self) {
        self.status.muted = self.muted;
        self.events.publish_app(UiEvent::VoiceStatusChanged {
            status: self.status.clone(),
        });
    }
}

async fn connect(connector: &dyn RealtimeConnector) -> Result<RealtimeConnection> {
    tokio::time::timeout(CONNECT_TIMEOUT, connector.connect())
        .await
        .map_err(|_| {
            anyhow!(
                "No realtime connection after {}s",
                CONNECT_TIMEOUT.as_secs()
            )
        })?
}

fn samples_to_ms(samples: u64) -> u64 {
    samples * 1000 / SAMPLE_RATE as u64
}

async fn sleep_until(timer: Option<(Timer, Instant)>) {
    match timer {
        Some((_, deadline)) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
