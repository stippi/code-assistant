use std::time::Duration;
use std::time::Instant;

use ratatui::text::Line;

use super::chunking::{AdaptiveChunkingPolicy, ChunkingDecision, DrainPlan, QueueSnapshot};
use super::StreamState;

pub struct CommitTickOutput {
    pub text_lines: Vec<Line<'static>>,
    pub thinking_lines: Vec<Line<'static>>,
}

pub fn run_commit_tick(
    policy: &mut AdaptiveChunkingPolicy,
    text_state: &mut StreamState,
    thinking_state: &mut StreamState,
    now: Instant,
) -> CommitTickOutput {
    let snapshot = stream_queue_snapshot(text_state, thinking_state, now);
    let decision = resolve_chunking_plan(policy, snapshot, now);
    let _ = decision.mode;
    apply_commit_tick_plan(decision.drain_plan, text_state, thinking_state)
}

fn stream_queue_snapshot(
    text_state: &StreamState,
    thinking_state: &StreamState,
    now: Instant,
) -> QueueSnapshot {
    let queued_lines = text_state.queued_len() + thinking_state.queued_len();
    let oldest_age = max_duration(
        text_state.oldest_queued_age(now),
        thinking_state.oldest_queued_age(now),
    );

    QueueSnapshot {
        queued_lines,
        oldest_age,
    }
}

fn resolve_chunking_plan(
    policy: &mut AdaptiveChunkingPolicy,
    snapshot: QueueSnapshot,
    now: Instant,
) -> ChunkingDecision {
    policy.decide(snapshot, now)
}

fn apply_commit_tick_plan(
    drain_plan: DrainPlan,
    text_state: &mut StreamState,
    thinking_state: &mut StreamState,
) -> CommitTickOutput {
    let text_lines = match drain_plan {
        DrainPlan::Single => text_state.drain_n(1),
        DrainPlan::Batch(max_lines) => text_state.drain_n(max_lines.max(1)),
    };
    let thinking_lines = match drain_plan {
        DrainPlan::Single => thinking_state.drain_n(1),
        DrainPlan::Batch(max_lines) => thinking_state.drain_n(max_lines.max(1)),
    };

    CommitTickOutput {
        text_lines,
        thinking_lines,
    }
}

fn max_duration(lhs: Option<Duration>, rhs: Option<Duration>) -> Option<Duration> {
    match (lhs, rhs) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}
