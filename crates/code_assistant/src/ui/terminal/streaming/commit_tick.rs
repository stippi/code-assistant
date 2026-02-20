use std::collections::VecDeque;
use std::time::Instant;

use super::chunking::{
    AdaptiveChunkingPolicy, ChunkingDecision, ChunkingMode, DrainPlan, QueueSnapshot,
};
use super::controller::QueuedChunk;

pub fn queue_snapshot(queue: &VecDeque<QueuedChunk>, now: Instant) -> QueueSnapshot {
    let oldest_age = queue
        .front()
        .map(|head| now.saturating_duration_since(head.enqueued_at));
    QueueSnapshot {
        queued_lines: queue.len(),
        oldest_age,
    }
}

pub fn resolve_drain_plan(
    policy: &mut AdaptiveChunkingPolicy,
    queue: &VecDeque<QueuedChunk>,
    now: Instant,
) -> ChunkingDecision {
    let snapshot = queue_snapshot(queue, now);
    policy.decide(snapshot, now)
}

pub fn apply_drain_plan(queue: &mut VecDeque<QueuedChunk>, plan: DrainPlan) -> Vec<QueuedChunk> {
    let drain_count = match plan {
        DrainPlan::Single => 1,
        DrainPlan::Batch(max_lines) => max_lines.max(1),
    };

    let mut drained = Vec::with_capacity(drain_count);
    for _ in 0..drain_count {
        let Some(chunk) = queue.pop_front() else {
            break;
        };
        drained.push(chunk);
    }
    drained
}

pub fn run_commit_tick(
    policy: &mut AdaptiveChunkingPolicy,
    queue: &mut VecDeque<QueuedChunk>,
    now: Instant,
) -> (Vec<QueuedChunk>, ChunkingMode) {
    let decision = resolve_drain_plan(policy, queue, now);
    let drained = apply_drain_plan(queue, decision.drain_plan);
    (drained, decision.mode)
}
