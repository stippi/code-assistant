use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::chunking::AdaptiveChunkingPolicy;
use super::commit_tick::run_commit_tick;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Text,
    Thinking,
}

#[derive(Debug, Clone)]
pub struct StreamDelta {
    pub kind: StreamKind,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct QueuedChunk {
    pub delta: StreamDelta,
    pub enqueued_at: Instant,
}

#[derive(Debug, Default, Clone)]
struct PartialBuffer {
    content: String,
    started_at: Option<Instant>,
}

impl PartialBuffer {
    fn clear(&mut self) {
        self.content.clear();
        self.started_at = None;
    }

    fn push(&mut self, content: String, now: Instant) {
        if self.content.is_empty() {
            self.started_at = Some(now);
        }
        self.content.push_str(&content);
    }

    fn take_complete_lines(&mut self) -> Option<String> {
        let last_newline = self.content.rfind('\n')?;
        let mut tail = self.content.split_off(last_newline + 1);
        std::mem::swap(&mut tail, &mut self.content);

        if self.content.is_empty() {
            self.started_at = None;
        }

        if tail.is_empty() {
            None
        } else {
            Some(tail)
        }
    }

    fn take_all(&mut self) -> Option<String> {
        if self.content.is_empty() {
            return None;
        }
        let mut all = String::new();
        std::mem::swap(&mut all, &mut self.content);
        self.started_at = None;
        Some(all)
    }

    fn should_flush_partial(&self, now: Instant) -> bool {
        self.started_at.is_some_and(|started| {
            now.saturating_duration_since(started) >= StreamingController::PARTIAL_FLUSH_AGE
        }) || self.content.len() >= StreamingController::PARTIAL_FLUSH_SIZE
    }
}

pub struct StreamingController {
    queue: VecDeque<QueuedChunk>,
    policy: AdaptiveChunkingPolicy,
    text_partial: PartialBuffer,
    thinking_partial: PartialBuffer,
}

impl StreamingController {
    const PARTIAL_FLUSH_AGE: Duration = Duration::from_millis(80);
    const PARTIAL_FLUSH_SIZE: usize = 1024;

    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            policy: AdaptiveChunkingPolicy::new(),
            text_partial: PartialBuffer::default(),
            thinking_partial: PartialBuffer::default(),
        }
    }

    pub fn clear(&mut self) {
        self.queue.clear();
        self.policy.reset();
        self.text_partial.clear();
        self.thinking_partial.clear();
    }

    pub fn push(&mut self, kind: StreamKind, content: String) {
        self.push_at(kind, content, Instant::now());
    }

    pub fn drain_commit_tick(&mut self) -> Vec<StreamDelta> {
        self.drain_commit_tick_at(Instant::now())
    }

    pub fn flush_pending(&mut self) -> Vec<StreamDelta> {
        self.flush_pending_at(Instant::now())
    }

    fn push_at(&mut self, kind: StreamKind, content: String, now: Instant) {
        if content.is_empty() {
            return;
        }

        let partial = self.partial_mut(kind);
        partial.push(content, now);
        if let Some(committed) = partial.take_complete_lines() {
            self.enqueue_split(kind, committed, now);
        }
    }

    fn drain_commit_tick_at(&mut self, now: Instant) -> Vec<StreamDelta> {
        self.flush_stale_partials(now);

        if self.queue.is_empty() {
            return Vec::new();
        }

        let (drained, _) = run_commit_tick(&mut self.policy, &mut self.queue, now);
        drained.into_iter().map(|chunk| chunk.delta).collect()
    }

    fn flush_pending_at(&mut self, now: Instant) -> Vec<StreamDelta> {
        self.flush_all_partials(now);
        let drained = self.queue.drain(..).map(|chunk| chunk.delta).collect();
        self.policy.reset();
        drained
    }

    fn flush_stale_partials(&mut self, now: Instant) {
        if self.text_partial.should_flush_partial(now) {
            if let Some(content) = self.text_partial.take_all() {
                self.enqueue(StreamKind::Text, content, now);
            }
        }

        if self.thinking_partial.should_flush_partial(now) {
            if let Some(content) = self.thinking_partial.take_all() {
                self.enqueue(StreamKind::Thinking, content, now);
            }
        }
    }

    fn flush_all_partials(&mut self, now: Instant) {
        if let Some(content) = self.text_partial.take_all() {
            self.enqueue(StreamKind::Text, content, now);
        }

        if let Some(content) = self.thinking_partial.take_all() {
            self.enqueue(StreamKind::Thinking, content, now);
        }
    }

    fn enqueue_split(&mut self, kind: StreamKind, content: String, enqueued_at: Instant) {
        for line in content.split_inclusive('\n') {
            self.enqueue(kind, line.to_string(), enqueued_at);
        }
    }

    fn enqueue(&mut self, kind: StreamKind, content: String, enqueued_at: Instant) {
        if content.is_empty() {
            return;
        }
        self.queue.push_back(QueuedChunk {
            delta: StreamDelta { kind, content },
            enqueued_at,
        });
    }

    fn partial_mut(&mut self, kind: StreamKind) -> &mut PartialBuffer {
        match kind {
            StreamKind::Text => &mut self.text_partial,
            StreamKind::Thinking => &mut self.thinking_partial,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newline_gating_commits_only_complete_lines() {
        let mut controller = StreamingController::new();
        let t0 = Instant::now();

        controller.push_at(StreamKind::Text, "hello".to_string(), t0);
        assert!(controller.queue.is_empty());

        controller.push_at(StreamKind::Text, " world\nnext".to_string(), t0);
        assert_eq!(controller.queue.len(), 1);

        let drained = controller.drain_commit_tick_at(t0);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].content, "hello world\n");

        assert!(controller.queue.is_empty());
        assert_eq!(controller.text_partial.content, "next");
    }

    #[test]
    fn commit_tick_flushes_old_partial_tail() {
        let mut controller = StreamingController::new();
        let t0 = Instant::now();

        controller.push_at(StreamKind::Thinking, "tail".to_string(), t0);
        assert!(controller.queue.is_empty());

        let drained = controller.drain_commit_tick_at(t0 + Duration::from_millis(120));
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].kind, StreamKind::Thinking);
        assert_eq!(drained[0].content, "tail");
    }

    #[test]
    fn flush_pending_drains_queue_and_partials() {
        let mut controller = StreamingController::new();
        let t0 = Instant::now();

        controller.push_at(StreamKind::Text, "line\n".to_string(), t0);
        controller.push_at(StreamKind::Text, "tail".to_string(), t0);

        let flushed = controller.flush_pending_at(t0);
        assert_eq!(flushed.len(), 2);
        assert_eq!(flushed[0].content, "line\n");
        assert_eq!(flushed[1].content, "tail");
        assert!(controller.queue.is_empty());
        assert!(controller.text_partial.content.is_empty());
    }
}
