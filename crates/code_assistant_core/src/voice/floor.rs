//! Who has the floor in a voice conversation, and when background work may
//! speak up.
//!
//! [`Floor`] is a pure state machine: [`FloorInput`]s go in, [`FloorCommand`]s
//! come out. It does no I/O and has no clock — timers are commands the voice
//! agent runs and reports back as [`FloorInput::TimerFired`].
//!
//! Two priority levels:
//!
//! 1. **Voice**: the user's speech, the model's answer and the tool calls of
//!    that answer.
//! 2. **Background**: notifications about conversations. They are only
//!    delivered on a free floor and never cancel or delay level 1.
//!
//! The model generates audio much faster than it plays, so `response.done`
//! does not mean the model stopped talking: the floor is free only once the
//! speakers drained ([`FloorInput::PlaybackDrained`], or the playback timer
//! as a fallback) and then stayed quiet for the cooling time.
//!
//! Invariants (each has a test below):
//!
//! 1. No `response.create` while a response is open.
//! 2. Neither a tool result nor a notification triggers a response while the
//!    user has the floor.
//! 3. Notifications flush only from `Idle`: after playback drained *and* the
//!    cooling time passed in silence.
//! 4. A notification never cancels a response or stops playback.
//! 5. Barge-in stops playback (the agent truncates the item to what was
//!    heard) and cancels the open response.

use super::notifications::{Notification, NotificationQueue};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct FloorConfig {
    /// Silence after playback before background notifications may speak.
    pub cooling: Duration,
    /// Extra wait beyond the expected playback end before assuming the
    /// speakers drained (in case the drain report is lost).
    pub playback_grace: Duration,
    /// How long after the user stopped speaking the floor stays theirs when
    /// no response follows.
    pub user_turn_grace: Duration,
}

impl Default for FloorConfig {
    fn default() -> Self {
        Self {
            cooling: Duration::from_millis(1500),
            playback_grace: Duration::from_secs(5),
            user_turn_grace: Duration::from_secs(8),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorState {
    /// Silent, nothing pending.
    Idle,
    /// A response is being generated or its audio is still playing.
    Speaking,
    /// The user is speaking, or spoke and is owed an answer.
    UserTurn,
    /// Playback drained; waiting for the cooling time of silence.
    Cooling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    /// Fallback for a lost playback drain report.
    Playback,
    /// Silence after playback.
    Cooling,
    /// The user stopped speaking but no response followed.
    UserTurn,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FloorInput {
    /// The server started a response (ours or one its turn detection made).
    ResponseCreated,
    /// The response finished generating; `pending_playback` is the audio of
    /// it still queued for the speakers.
    ResponseDone {
        pending_playback: Duration,
    },
    /// The user started speaking.
    SpeechStarted,
    /// The user stopped speaking.
    SpeechStopped,
    /// The speakers played everything queued.
    PlaybackDrained,
    /// The server reported an error.
    ServerError,
    /// A tool call started running.
    ToolStarted,
    /// A tool call finished.
    ToolCompleted {
        call_id: String,
        output: String,
    },
    Notification(Notification),
    /// A conversation's wait for the user ended elsewhere.
    WaitEnded {
        conversation_id: String,
    },
    /// The model read a conversation, so its notification is known.
    ConversationRead {
        conversation_id: String,
    },
    TimerFired(Timer),
}

#[derive(Debug, Clone, PartialEq)]
pub enum FloorCommand {
    /// `response.cancel`.
    CancelResponse,
    /// Drop queued audio and truncate the current item to what was heard.
    StopPlayback,
    /// `conversation.item.create` with a function call output.
    SendToolOutput {
        call_id: String,
        output: String,
    },
    /// `conversation.item.create` with a background system message.
    SendNotification(String),
    /// `response.create`.
    CreateResponse,
    /// Arm the (single) timer, replacing an armed one.
    StartTimer(Timer, Duration),
    CancelTimer,
}

#[derive(Debug)]
pub struct Floor {
    config: FloorConfig,
    state: FloorState,
    /// A response is being generated (or we asked for one).
    response_open: bool,
    /// We sent `response.create` and wait for `response.created`.
    create_pending: bool,
    /// The user barged in while our `response.create` was in flight: cancel
    /// that response as soon as it exists.
    cancel_on_create: bool,
    /// Tool outputs that completed while a response was open; attached at
    /// `response.done`.
    held_outputs: Vec<(String, String)>,
    /// Outputs were attached after generation; respond once playback ends.
    pending_trigger: bool,
    tools_running: usize,
    armed: Option<Timer>,
    queue: NotificationQueue,
}

impl Floor {
    pub fn new(config: FloorConfig) -> Self {
        Self {
            config,
            state: FloorState::Idle,
            response_open: false,
            create_pending: false,
            cancel_on_create: false,
            held_outputs: Vec::new(),
            pending_trigger: false,
            tools_running: 0,
            armed: None,
            queue: NotificationQueue::default(),
        }
    }

    pub fn state(&self) -> FloorState {
        self.state
    }

    pub fn queued_notifications(&self) -> usize {
        self.queue.len()
    }

    /// The realtime session was replaced by a fresh one: everything tied to
    /// the old one (open response, tool calls, held outputs) is gone. Queued
    /// notifications stay and go out after a cooling period.
    pub fn reconnected(&mut self) -> Vec<FloorCommand> {
        let queue = std::mem::take(&mut self.queue);
        *self = Self::new(self.config.clone());
        self.queue = queue;
        let mut out = vec![FloorCommand::CancelTimer];
        if !self.queue.is_empty() {
            self.state = FloorState::Cooling;
            self.start_timer(Timer::Cooling, self.config.cooling, &mut out);
        }
        out
    }

    pub fn handle(&mut self, input: FloorInput) -> Vec<FloorCommand> {
        let mut out = Vec::new();
        match input {
            FloorInput::ResponseCreated => {
                self.create_pending = false;
                self.response_open = true;
                self.cancel_timer(&mut out);
                if self.cancel_on_create {
                    self.cancel_on_create = false;
                    out.push(FloorCommand::CancelResponse);
                } else {
                    self.state = FloorState::Speaking;
                }
            }
            FloorInput::ResponseDone { pending_playback } => {
                self.response_open = false;
                self.create_pending = false;
                self.cancel_on_create = false;
                let attached = !self.held_outputs.is_empty();
                for (call_id, output) in self.held_outputs.drain(..) {
                    out.push(FloorCommand::SendToolOutput { call_id, output });
                }
                // Over the user the model folds the outputs into its next
                // answer instead (invariant 2).
                if attached && self.state != FloorState::UserTurn {
                    self.pending_trigger = true;
                }
                if self.state == FloorState::Speaking {
                    if pending_playback.is_zero() {
                        self.playback_finished(&mut out);
                    } else {
                        self.start_timer(
                            Timer::Playback,
                            pending_playback + self.config.playback_grace,
                            &mut out,
                        );
                    }
                }
            }
            FloorInput::SpeechStarted => {
                self.cancel_timer(&mut out);
                if self.state == FloorState::Speaking {
                    if self.create_pending {
                        self.cancel_on_create = true;
                    } else if self.response_open {
                        out.push(FloorCommand::CancelResponse);
                    }
                    out.push(FloorCommand::StopPlayback);
                }
                self.pending_trigger = false;
                self.state = FloorState::UserTurn;
            }
            FloorInput::SpeechStopped => {
                if self.state == FloorState::UserTurn {
                    self.start_timer(Timer::UserTurn, self.config.user_turn_grace, &mut out);
                }
            }
            FloorInput::PlaybackDrained => {
                if self.waiting_for_playback() {
                    self.playback_finished(&mut out);
                }
            }
            FloorInput::ServerError => {
                if self.create_pending {
                    self.create_pending = false;
                    self.response_open = false;
                    self.cancel_on_create = false;
                    if self.state == FloorState::Speaking {
                        self.playback_finished(&mut out);
                    }
                }
            }
            FloorInput::ToolStarted => self.tools_running += 1,
            FloorInput::ToolCompleted { call_id, output } => {
                self.tools_running = self.tools_running.saturating_sub(1);
                if self.response_open {
                    self.held_outputs.push((call_id, output));
                } else {
                    out.push(FloorCommand::SendToolOutput { call_id, output });
                    match self.state {
                        FloorState::Speaking => self.pending_trigger = true,
                        FloorState::UserTurn => {}
                        FloorState::Cooling | FloorState::Idle => {
                            self.cancel_timer(&mut out);
                            self.create_response(&mut out);
                        }
                    }
                }
            }
            FloorInput::Notification(notification) => {
                self.queue.push(notification);
                match self.state {
                    // Notifications finishing close together go out as one.
                    FloorState::Cooling => {
                        self.start_timer(Timer::Cooling, self.config.cooling, &mut out)
                    }
                    FloorState::Idle => self.flush(&mut out),
                    FloorState::Speaking | FloorState::UserTurn => {}
                }
            }
            FloorInput::WaitEnded { conversation_id } => {
                self.queue.retract_waiting(&conversation_id)
            }
            FloorInput::ConversationRead { conversation_id } => {
                self.queue.acknowledge(&conversation_id)
            }
            FloorInput::TimerFired(timer) => {
                if self.armed != Some(timer) {
                    return out;
                }
                self.armed = None;
                match timer {
                    Timer::Playback => {
                        if self.waiting_for_playback() {
                            self.playback_finished(&mut out);
                        }
                    }
                    Timer::Cooling if self.state == FloorState::Cooling => {
                        self.state = FloorState::Idle;
                        self.flush(&mut out);
                    }
                    Timer::UserTurn if self.state == FloorState::UserTurn => {
                        self.state = FloorState::Idle;
                        self.flush(&mut out);
                    }
                    Timer::Cooling | Timer::UserTurn => {}
                }
            }
        }
        out
    }

    fn waiting_for_playback(&self) -> bool {
        self.state == FloorState::Speaking && !self.response_open && !self.create_pending
    }

    fn playback_finished(&mut self, out: &mut Vec<FloorCommand>) {
        self.cancel_timer(out);
        if self.pending_trigger {
            self.pending_trigger = false;
            self.create_response(out);
        } else {
            self.state = FloorState::Cooling;
            self.start_timer(Timer::Cooling, self.config.cooling, out);
        }
    }

    /// Deliver queued notifications, only from a free floor and not while
    /// a tool result is about to make the model speak anyway.
    fn flush(&mut self, out: &mut Vec<FloorCommand>) {
        if self.state != FloorState::Idle || self.tools_running > 0 {
            return;
        }
        if let Some(message) = self.queue.take_message() {
            out.push(FloorCommand::SendNotification(message));
            self.create_response(out);
        }
    }

    fn create_response(&mut self, out: &mut Vec<FloorCommand>) {
        debug_assert!(!self.response_open, "invariant 1: one response at a time");
        self.create_pending = true;
        self.response_open = true;
        self.state = FloorState::Speaking;
        out.push(FloorCommand::CreateResponse);
    }

    fn start_timer(&mut self, timer: Timer, after: Duration, out: &mut Vec<FloorCommand>) {
        self.armed = Some(timer);
        out.push(FloorCommand::StartTimer(timer, after));
    }

    fn cancel_timer(&mut self, out: &mut Vec<FloorCommand>) {
        if self.armed.take().is_some() {
            out.push(FloorCommand::CancelTimer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::notifications::NotificationKind;
    use super::*;
    use FloorCommand as C;
    use FloorInput as I;

    fn floor() -> Floor {
        Floor::new(FloorConfig::default())
    }

    fn finished(id: &str) -> I {
        I::Notification(Notification {
            conversation_id: id.into(),
            name: id.into(),
            project: String::new(),
            kind: NotificationKind::Finished,
        })
    }

    fn done(ms: u64) -> I {
        I::ResponseDone {
            pending_playback: Duration::from_millis(ms),
        }
    }

    fn completed(call: &str) -> I {
        I::ToolCompleted {
            call_id: call.into(),
            output: "out".into(),
        }
    }

    fn creates(commands: &[C]) -> bool {
        commands.contains(&C::CreateResponse)
    }

    fn notifies(commands: &[C]) -> bool {
        commands.iter().any(|c| matches!(c, C::SendNotification(_)))
    }

    /// Run a sequence and return the commands of the last input.
    fn run(floor: &mut Floor, inputs: Vec<I>) -> Vec<C> {
        let mut last = Vec::new();
        for input in inputs {
            last = floor.handle(input);
        }
        last
    }

    /// A model answer that played to the end and cooled down.
    fn answered_and_cooled(floor: &mut Floor) {
        run(
            floor,
            vec![
                I::ResponseCreated,
                done(800),
                I::PlaybackDrained,
                I::TimerFired(Timer::Cooling),
            ],
        );
        assert_eq!(floor.state(), FloorState::Idle);
    }

    #[test]
    fn idle_floor_flushes_a_notification_at_once() {
        let mut floor = floor();
        let out = floor.handle(finished("a"));
        assert!(notifies(&out));
        assert!(creates(&out));
        assert_eq!(floor.state(), FloorState::Speaking);
    }

    #[test]
    fn notification_while_speaking_waits_for_drain_and_cooling() {
        // Invariants 3 and 4.
        let mut floor = floor();
        floor.handle(I::ResponseCreated);
        let out = floor.handle(finished("a"));
        assert!(out.is_empty(), "never interrupts the model: {out:?}");

        let out = floor.handle(done(2000));
        assert_eq!(
            out,
            vec![C::StartTimer(Timer::Playback, Duration::from_millis(7000))]
        );
        // Generation is done, the audio still plays.
        assert_eq!(floor.state(), FloorState::Speaking);

        let out = floor.handle(I::PlaybackDrained);
        assert!(!notifies(&out));
        assert_eq!(floor.state(), FloorState::Cooling);
        assert!(out.contains(&C::StartTimer(Timer::Cooling, Duration::from_millis(1500))));

        let out = floor.handle(I::TimerFired(Timer::Cooling));
        assert!(notifies(&out) && creates(&out));
    }

    #[test]
    fn notification_during_user_turn_never_triggers() {
        // Invariant 2.
        let mut floor = floor();
        floor.handle(I::SpeechStarted);
        assert!(floor.handle(finished("a")).is_empty());
        floor.handle(I::SpeechStopped);
        // The server answers the user; the notification waits for that.
        assert!(!creates(&floor.handle(I::ResponseCreated)));
        assert!(!creates(&floor.handle(done(0))));
        let out = floor.handle(I::TimerFired(Timer::Cooling));
        assert!(notifies(&out));
    }

    #[test]
    fn notifications_close_together_go_out_as_one() {
        let mut floor = floor();
        run(&mut floor, vec![I::ResponseCreated, done(0)]);
        assert_eq!(floor.state(), FloorState::Cooling);
        let out = floor.handle(finished("a"));
        assert_eq!(
            out,
            vec![C::StartTimer(Timer::Cooling, Duration::from_millis(1500))],
            "restarts the silence timer"
        );
        floor.handle(finished("b"));
        let out = floor.handle(I::TimerFired(Timer::Cooling));
        let messages: Vec<_> = out
            .iter()
            .filter_map(|c| match c {
                C::SendNotification(m) => Some(m),
                _ => None,
            })
            .collect();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("id a") && messages[0].contains("id b"));
        assert_eq!(out.iter().filter(|c| **c == C::CreateResponse).count(), 1);
    }

    #[test]
    fn barge_in_cancels_the_response_and_stops_playback() {
        // Invariant 5.
        let mut floor = floor();
        floor.handle(I::ResponseCreated);
        let out = floor.handle(I::SpeechStarted);
        assert!(out.contains(&C::CancelResponse));
        assert!(out.contains(&C::StopPlayback));
        assert_eq!(floor.state(), FloorState::UserTurn);
    }

    #[test]
    fn barge_in_after_generation_only_stops_playback() {
        let mut floor = floor();
        run(&mut floor, vec![I::ResponseCreated, done(3000)]);
        let out = floor.handle(I::SpeechStarted);
        assert!(!out.contains(&C::CancelResponse), "nothing to cancel");
        assert!(out.contains(&C::StopPlayback));
        // The playback fallback timer must not fire into the user's turn.
        assert!(out.contains(&C::CancelTimer));
        assert!(floor.handle(I::TimerFired(Timer::Playback)).is_empty());
    }

    #[test]
    fn barge_in_before_our_response_exists_cancels_it_on_creation() {
        let mut floor = floor();
        floor.handle(finished("a")); // flushes, response.create in flight
        let out = floor.handle(I::SpeechStarted);
        assert!(!out.contains(&C::CancelResponse));
        let out = floor.handle(I::ResponseCreated);
        assert_eq!(out, vec![C::CancelResponse]);
        assert_eq!(floor.state(), FloorState::UserTurn);
    }

    #[test]
    fn tool_call_round_trip_triggers_a_follow_up_response() {
        let mut floor = floor();
        // The model's response is just a function call; the tool runs after
        // it is done.
        run(
            &mut floor,
            vec![I::ResponseCreated, I::ToolStarted, done(0)],
        );
        assert_eq!(floor.state(), FloorState::Cooling);
        let out = floor.handle(completed("c1"));
        assert_eq!(
            out,
            vec![
                C::SendToolOutput {
                    call_id: "c1".into(),
                    output: "out".into()
                },
                C::CancelTimer,
                C::CreateResponse
            ]
        );
    }

    #[test]
    fn tool_output_during_generation_is_held_until_done() {
        // Invariant 1.
        let mut floor = floor();
        run(&mut floor, vec![I::ResponseCreated, I::ToolStarted]);
        assert!(floor.handle(completed("c1")).is_empty());
        let out = floor.handle(done(0));
        assert_eq!(
            out,
            vec![
                C::SendToolOutput {
                    call_id: "c1".into(),
                    output: "out".into()
                },
                C::CreateResponse
            ]
        );
    }

    #[test]
    fn tool_output_while_audio_plays_responds_after_drain() {
        let mut floor = floor();
        run(
            &mut floor,
            vec![I::ResponseCreated, I::ToolStarted, done(1200)],
        );
        let out = floor.handle(completed("c1"));
        assert!(!creates(&out), "the speakers still play: {out:?}");
        let out = floor.handle(I::PlaybackDrained);
        assert!(creates(&out));
    }

    #[test]
    fn tool_output_over_the_user_attaches_without_trigger() {
        // Invariant 2.
        let mut floor = floor();
        run(
            &mut floor,
            vec![
                I::ResponseCreated,
                I::ToolStarted,
                done(0),
                I::SpeechStarted,
            ],
        );
        let out = floor.handle(completed("c1"));
        assert_eq!(
            out,
            vec![C::SendToolOutput {
                call_id: "c1".into(),
                output: "out".into()
            }]
        );
    }

    #[test]
    fn barge_in_drops_a_pending_trigger() {
        let mut floor = floor();
        run(
            &mut floor,
            vec![
                I::ResponseCreated,
                I::ToolStarted,
                done(1200),
                completed("c1"),
            ],
        );
        floor.handle(I::SpeechStarted);
        let out = run(&mut floor, vec![I::SpeechStopped, I::PlaybackDrained]);
        assert!(!creates(&out));
    }

    #[test]
    fn running_tools_hold_notifications_back() {
        let mut floor = floor();
        run(
            &mut floor,
            vec![I::ResponseCreated, I::ToolStarted, done(0)],
        );
        floor.handle(finished("a"));
        let out = floor.handle(I::TimerFired(Timer::Cooling));
        assert!(!notifies(&out), "the tool result speaks first");
        let out = floor.handle(completed("c1"));
        assert!(!notifies(&out) && creates(&out));
        let out = run(
            &mut floor,
            vec![I::ResponseCreated, done(0), I::TimerFired(Timer::Cooling)],
        );
        assert!(notifies(&out));
    }

    #[test]
    fn lost_drain_report_falls_back_to_the_playback_timer() {
        let mut floor = floor();
        run(&mut floor, vec![I::ResponseCreated, done(500)]);
        floor.handle(I::TimerFired(Timer::Playback));
        assert_eq!(floor.state(), FloorState::Cooling);
    }

    #[test]
    fn drain_during_generation_is_an_underrun_not_the_end() {
        let mut floor = floor();
        floor.handle(I::ResponseCreated);
        assert!(floor.handle(I::PlaybackDrained).is_empty());
        assert_eq!(floor.state(), FloorState::Speaking);
    }

    #[test]
    fn silent_user_turn_returns_the_floor() {
        let mut floor = floor();
        run(&mut floor, vec![I::SpeechStarted, finished("a")]);
        let out = floor.handle(I::SpeechStopped);
        assert_eq!(
            out,
            vec![C::StartTimer(Timer::UserTurn, Duration::from_secs(8))]
        );
        let out = floor.handle(I::TimerFired(Timer::UserTurn));
        assert!(notifies(&out));
    }

    #[test]
    fn failed_create_does_not_wedge_the_floor() {
        let mut floor = floor();
        floor.handle(finished("a"));
        floor.handle(I::ServerError);
        assert_eq!(floor.state(), FloorState::Cooling);
        floor.handle(I::TimerFired(Timer::Cooling));
        assert_eq!(floor.state(), FloorState::Idle);
        assert!(creates(&floor.handle(finished("b"))));
    }

    #[test]
    fn reading_a_conversation_drops_its_notification() {
        let mut floor = floor();
        floor.handle(I::ResponseCreated);
        floor.handle(finished("a"));
        floor.handle(I::ConversationRead {
            conversation_id: "a".into(),
        });
        let out = run(&mut floor, vec![done(0), I::TimerFired(Timer::Cooling)]);
        assert!(!notifies(&out));
        assert_eq!(floor.queued_notifications(), 0);
    }

    #[test]
    fn stale_timer_fires_are_ignored() {
        let mut floor = floor();
        answered_and_cooled(&mut floor);
        assert!(floor.handle(I::TimerFired(Timer::Cooling)).is_empty());
        assert!(floor.handle(I::TimerFired(Timer::Playback)).is_empty());
    }
}
