//! End-to-end tests of the voice agent: a scripted realtime server, fake
//! audio devices, and a real session service whose agents answer from a
//! mock LLM.

use super::*;
use crate::mocks::{MockLLMProvider, create_test_response_text};
use crate::persistence::FileSessionPersistence;
use crate::session::event_stream::EventStream;
use crate::session::service::{AgentRuntimeOptions, default_project_manager_factory};
use crate::session::{SessionConfig, SessionManager, SessionService};
use crate::ui::UiEvent;
use llm::realtime::{
    ClientEvent, ConversationItem, Incoming, RealtimeConnection, RealtimeConnector, ResponseInfo,
    ServerEvent, encode_pcm16,
};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc;

const WAIT: Duration = Duration::from_secs(5);

/// The server side of one scripted realtime connection.
struct Server {
    from_client: mpsc::UnboundedReceiver<ClientEvent>,
    to_client: mpsc::UnboundedSender<Incoming>,
}

impl Server {
    fn send(&self, event: ServerEvent) {
        self.to_client.send(Incoming::Event(event)).unwrap();
    }

    /// Read client events until one matches; audio appends are skipped.
    async fn expect(&mut self, what: &str, pred: impl Fn(&ClientEvent) -> bool) -> ClientEvent {
        tokio::time::timeout(WAIT, async {
            loop {
                let event = self.from_client.recv().await.expect("client hung up");
                if pred(&event) {
                    return event;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
    }

    /// Assert that nothing but audio arrives for `duration`.
    async fn expect_quiet(&mut self, duration: Duration) {
        let deadline = tokio::time::Instant::now() + duration;
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, self.from_client.recv()).await
        {
            assert!(
                matches!(event, ClientEvent::AppendAudio(_)),
                "expected no client events, got {event:?}"
            );
        }
    }
}

struct ScriptedConnector {
    connections: Mutex<Vec<RealtimeConnection>>,
}

#[async_trait::async_trait]
impl RealtimeConnector for ScriptedConnector {
    async fn connect(&self) -> anyhow::Result<RealtimeConnection> {
        self.connections
            .lock()
            .unwrap()
            .pop()
            .ok_or_else(|| anyhow::anyhow!("no more scripted connections"))
    }
}

fn scripted_connection() -> (RealtimeConnection, Server) {
    let (outgoing, from_client) = mpsc::unbounded_channel();
    let (to_client, incoming) = mpsc::unbounded_channel();
    (
        RealtimeConnection { outgoing, incoming },
        Server {
            from_client,
            to_client,
        },
    )
}

#[derive(Default)]
struct AudioState {
    queued: u64,
    played: u64,
    clears: usize,
    events: Option<mpsc::UnboundedSender<AudioEvent>>,
}

#[derive(Clone, Default)]
struct FakeAudio(Arc<Mutex<AudioState>>);

impl FakeAudio {
    fn factory(&self) -> AudioFactory {
        let audio = self.clone();
        Arc::new(move |events| {
            audio.0.lock().unwrap().events = Some(events);
            Ok(Box::new(audio.clone()) as Box<dyn VoiceAudio>)
        })
    }

    /// Wait until the voice agent, on its own task, brought the devices
    /// into the state `pred` expects.
    async fn wait_for(&self, what: &str, pred: impl Fn(&AudioState) -> bool) {
        tokio::time::timeout(WAIT, async {
            while !pred(&self.0.lock().unwrap()) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    /// The speakers played this many samples, and drained if that was all.
    fn play(&self, samples: u64) {
        let mut state = self.0.lock().unwrap();
        state.played = (state.played + samples).min(state.queued);
        if state.played == state.queued {
            let _ = state.events.as_ref().unwrap().send(AudioEvent::Drained);
        }
    }
}

impl VoiceAudio for FakeAudio {
    fn play(&mut self, samples: Vec<i16>) {
        self.0.lock().unwrap().queued += samples.len() as u64;
    }
    fn clear_playback(&mut self) {
        let mut state = self.0.lock().unwrap();
        state.clears += 1;
        state.queued = state.played;
    }
    fn played_samples(&self) -> u64 {
        self.0.lock().unwrap().played
    }
    fn set_capture_muted(&mut self, _muted: bool) {}
}

struct Harness {
    sessions: SessionService,
    voice: VoiceService,
    server: Server,
    audio: FakeAudio,
    _dir: tempfile::TempDir,
}

/// Never answers, like a connection attempt into a blackholed network.
struct HangingConnector;

#[async_trait::async_trait]
impl RealtimeConnector for HangingConnector {
    async fn connect(&self) -> anyhow::Result<RealtimeConnection> {
        std::future::pending().await
    }
}

fn test_config() -> VoiceConfig {
    VoiceConfig {
        provider: "test".into(),
        cooling_ms: 50,
        ..VoiceConfig::default()
    }
}

/// A voice service whose connection attempts never finish, with a
/// subscription to its status events.
fn hanging_voice(
    dir: &tempfile::TempDir,
) -> (VoiceService, crate::session::event_stream::Subscription) {
    let (sessions, events) = session_service(dir);
    let statuses = sessions.subscribe();
    let (voice, worker) = VoiceService::with_config_loader(
        sessions,
        events,
        FakeAudio::default().factory(),
        Arc::new(|_| Ok(Arc::new(HangingConnector) as Arc<dyn RealtimeConnector>)),
        Arc::new(test_config),
    );
    tokio::spawn(worker);
    (voice, statuses)
}

/// A session service whose agent runs all answer "done".
fn session_service(dir: &tempfile::TempDir) -> (SessionService, EventStream) {
    let events = EventStream::new();
    let manager = Arc::new(tokio::sync::Mutex::new(SessionManager::new(
        FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf()),
        SessionConfig::default(),
        "test-model".to_string(),
        crate::tools::test_registry(),
        events.clone(),
    )));
    let runtime = Arc::new(AgentRuntimeOptions {
        record_path: None,
        playback_path: None,
        fast_playback: false,
        command_executor_factory: Arc::new(|_| {
            Box::new(crate::mocks::create_command_executor_mock())
        }),
        project_manager_factory: default_project_manager_factory(),
        llm_client_factory: Some(Arc::new(|_| {
            Ok(Box::new(
                MockLLMProvider::new(vec![Ok(create_test_response_text("done"))]).streaming(),
            ))
        })),
    });
    let (sessions, worker) = SessionService::new(manager, runtime, events.clone());
    tokio::spawn(worker);
    (sessions, events)
}

/// A running voice session over [`session_service`].
async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, events) = session_service(&dir);
    let (connection, mut server) = scripted_connection();
    let connector: Arc<dyn RealtimeConnector> = Arc::new(ScriptedConnector {
        connections: Mutex::new(vec![connection]),
    });
    let audio = FakeAudio::default();
    let (voice, voice_worker) = VoiceService::with_config_loader(
        sessions.clone(),
        events,
        audio.factory(),
        Arc::new(move |_| Ok(connector.clone())),
        Arc::new(test_config),
    );
    tokio::spawn(voice_worker);
    voice.start();
    server
        .expect("session.update", |e| {
            matches!(e, ClientEvent::SessionUpdate(_))
        })
        .await;
    server.send(ServerEvent::SessionCreated);
    Harness {
        sessions,
        voice,
        server,
        audio,
        _dir: dir,
    }
}

fn response_created() -> ServerEvent {
    ServerEvent::ResponseCreated {
        response: ResponseInfo {
            id: "r".into(),
            status: None,
        },
    }
}

fn response_done() -> ServerEvent {
    ServerEvent::ResponseDone {
        response: ResponseInfo {
            id: "r".into(),
            status: Some("completed".into()),
        },
    }
}

fn audio_delta(item: &str, samples: usize) -> ServerEvent {
    ServerEvent::AudioDelta {
        item_id: item.into(),
        delta: encode_pcm16(&vec![0; samples]),
    }
}

fn function_call(name: &str, call_id: &str, arguments: serde_json::Value) -> ServerEvent {
    ServerEvent::OutputItemDone {
        item: ConversationItem {
            id: format!("item-{call_id}"),
            kind: "function_call".into(),
            name: Some(name.into()),
            call_id: Some(call_id.into()),
            arguments: Some(arguments.to_string()),
        },
    }
}

async fn wait_idle(sessions: &SessionService, id: &str) {
    tokio::time::timeout(WAIT, async {
        while sessions.is_session_busy(id.to_string()).await.unwrap() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("session finished");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finish_while_the_model_speaks_waits_for_playback_and_silence() {
    let mut h = harness().await;
    let id = h
        .sessions
        .create_session(Some("Fix flaky test".into()), None)
        .await
        .unwrap();

    // The model is talking: one second of audio is queued.
    h.server.send(response_created());
    h.server.send(audio_delta("speech", 24_000));
    h.audio
        .wait_for("the queued audio", |a| a.queued == 24_000)
        .await;
    h.sessions
        .send_user_message(id.clone(), "go".into(), Vec::new(), None)
        .await
        .unwrap();
    wait_idle(&h.sessions, &id).await;

    // Generation ends, the speakers still play.
    h.server.send(response_done());
    h.server.expect_quiet(Duration::from_millis(200)).await;

    h.audio.play(24_000);
    let note = h
        .server
        .expect("notification", |e| {
            matches!(e, ClientEvent::SystemMessage(_))
        })
        .await;
    let ClientEvent::SystemMessage(text) = note else {
        unreachable!()
    };
    assert!(text.contains("Fix flaky test"), "{text}");
    assert!(text.contains("finished"), "{text}");
    h.server
        .expect("response.create", |e| *e == ClientEvent::CreateResponse)
        .await;
    h.voice.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn message_conversation_starts_a_turn_and_its_end_is_announced() {
    let mut h = harness().await;
    // A session in "proj" makes the project known.
    h.sessions
        .create_session(None, Some("proj".into()))
        .await
        .unwrap();

    h.server.send(response_created());
    h.server.send(function_call(
        "message_conversation",
        "c1",
        serde_json::json!({"project": "proj", "title": "Docs", "message": "update the docs"}),
    ));
    h.server.send(response_done());

    let output = h
        .server
        .expect("tool output", |e| {
            matches!(e, ClientEvent::FunctionCallOutput { .. })
        })
        .await;
    let ClientEvent::FunctionCallOutput { call_id, output } = output else {
        unreachable!()
    };
    assert_eq!(call_id, "c1");
    let output: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["status"], "started");
    let new_id = output["conversation_id"].as_str().unwrap().to_string();
    // The output arrived on a free floor: the model answers at once.
    h.server
        .expect("response.create", |e| *e == ClientEvent::CreateResponse)
        .await;
    h.server.send(response_created());
    h.server.send(response_done());

    wait_idle(&h.sessions, &new_id).await;
    let note = h
        .server
        .expect("notification", |e| {
            matches!(e, ClientEvent::SystemMessage(_))
        })
        .await;
    let ClientEvent::SystemMessage(text) = note else {
        unreachable!()
    };
    assert!(text.contains(&new_id), "{text}");
    h.voice.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn reading_a_finished_conversation_returns_its_last_answer_and_drops_the_notification() {
    let mut h = harness().await;
    let id = h.sessions.create_session(None, None).await.unwrap();

    // The user talks while the conversation finishes.
    h.server.send(ServerEvent::SpeechStarted);
    h.sessions
        .send_user_message(id.clone(), "go".into(), Vec::new(), None)
        .await
        .unwrap();
    wait_idle(&h.sessions, &id).await;
    h.server.send(ServerEvent::SpeechStopped);
    h.server.send(response_created());
    h.server.send(function_call(
        "get_conversation",
        "c1",
        serde_json::json!({"conversation_id": id, "full": true}),
    ));
    h.server.send(response_done());

    let output = h
        .server
        .expect("tool output", |e| {
            matches!(e, ClientEvent::FunctionCallOutput { .. })
        })
        .await;
    let ClientEvent::FunctionCallOutput { output, .. } = output else {
        unreachable!()
    };
    let output: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["state"], "idle");
    assert_eq!(output["turns"][0]["user"], "go");
    assert_eq!(output["turns"][0]["agent"], "done");

    h.server
        .expect("response.create", |e| *e == ClientEvent::CreateResponse)
        .await;
    h.server.send(response_created());
    h.server.send(response_done());
    // The model knows the result; no notification follows.
    h.server.expect_quiet(Duration::from_millis(300)).await;
    h.voice.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn barge_in_cancels_and_truncates_to_what_was_heard() {
    let mut h = harness().await;
    h.server.send(response_created());
    h.server.send(audio_delta("speech", 24_000));
    h.audio
        .wait_for("the queued audio", |a| a.queued == 24_000)
        .await;
    // Half a second played when the user interrupts.
    h.audio.play(12_000);
    h.server.send(ServerEvent::SpeechStarted);

    h.server
        .expect("response.cancel", |e| *e == ClientEvent::CancelResponse)
        .await;
    let truncate = h
        .server
        .expect("truncate", |e| {
            matches!(e, ClientEvent::TruncateItem { .. })
        })
        .await;
    assert_eq!(
        truncate,
        ClientEvent::TruncateItem {
            item_id: "speech".into(),
            audio_end_ms: 500
        }
    );
    // The agent clears the speakers right after it sent the truncate.
    h.audio
        .wait_for("the cleared speakers", |a| a.clears == 1)
        .await;
    h.voice.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_response_cancelled_before_its_audio_stays_silent() {
    let mut h = harness().await;
    let id = h.sessions.create_session(None, None).await.unwrap();
    h.sessions
        .send_user_message(id.clone(), "go".into(), Vec::new(), None)
        .await
        .unwrap();
    wait_idle(&h.sessions, &id).await;
    // The finish is announced on the free floor...
    h.server
        .expect("response.create", |e| *e == ClientEvent::CreateResponse)
        .await;
    // ...and the user speaks up before that response exists.
    h.server.send(ServerEvent::SpeechStarted);
    h.server.send(response_created());
    h.server
        .expect("response.cancel", |e| *e == ClientEvent::CancelResponse)
        .await;
    // Audio generated before the server handled the cancel.
    h.server.send(audio_delta("notification", 2_400));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(h.audio.0.lock().unwrap().queued, 0, "played over the user");
    h.voice.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn status_events_follow_the_floor() {
    let h = harness().await;
    let mut events = h.sessions.subscribe();
    h.server.send(response_created());
    let status =
        wait_for_status(&mut events, WAIT, |s| s.activity == VoiceActivity::Speaking).await;
    assert!(!status.muted);
    h.voice.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_ends_a_connection_attempt_that_hangs() {
    let dir = tempfile::tempdir().unwrap();
    let (voice, mut statuses) = hanging_voice(&dir);
    voice.start();
    wait_for_status(&mut statuses, WAIT, |s| {
        s.activity == VoiceActivity::Connecting
    })
    .await;
    voice.stop();
    wait_for_status(&mut statuses, WAIT, |s| s.activity == VoiceActivity::Off).await;
}

async fn wait_for_status(
    events: &mut crate::session::event_stream::Subscription,
    within: Duration,
    pred: impl Fn(&VoiceStatus) -> bool,
) -> VoiceStatus {
    tokio::time::timeout(within, async {
        loop {
            if let Ok(event) = events.recv().await
                && let crate::session::event_stream::EventPayload::Ui(UiEvent::VoiceStatusChanged {
                    status,
                }) = event.payload
                && pred(&status)
            {
                return status;
            }
        }
    })
    .await
    .expect("voice status")
}

#[tokio::test(start_paused = true)]
async fn a_connection_attempt_that_hangs_times_out() {
    let dir = tempfile::tempdir().unwrap();
    let (voice, mut statuses) = hanging_voice(&dir);
    voice.start();
    // Paused time runs ahead to the connect timeout.
    let status = wait_for_status(&mut statuses, Duration::from_secs(60), |s| {
        matches!(s.activity, VoiceActivity::Failed(_))
    })
    .await;
    let VoiceActivity::Failed(message) = status.activity else {
        unreachable!()
    };
    assert!(message.contains("No realtime connection"), "{message}");
}
