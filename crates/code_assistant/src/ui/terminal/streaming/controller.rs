use std::time::Instant;

use ratatui::text::Line;

use super::chunking::AdaptiveChunkingPolicy;
use super::commit_tick::{run_commit_tick, CommitTickOutput};
use super::StreamState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamKind {
    Text,
    Thinking,
}

#[derive(Debug, Default)]
pub struct DrainedLines {
    pub text: Vec<Line<'static>>,
    pub thinking: Vec<Line<'static>>,
}

pub struct StreamingController {
    text_state: StreamState,
    thinking_state: StreamState,
    policy: AdaptiveChunkingPolicy,
}

impl StreamingController {
    pub fn new() -> Self {
        Self {
            text_state: StreamState::new(None),
            thinking_state: StreamState::new(None),
            policy: AdaptiveChunkingPolicy::new(),
        }
    }

    pub fn clear(&mut self) {
        self.text_state.clear();
        self.thinking_state.clear();
        self.policy.reset();
    }

    pub fn set_width(&mut self, width: Option<usize>) {
        self.text_state.set_width(width);
        self.thinking_state.set_width(width);
    }

    pub fn push(&mut self, kind: StreamKind, content: String) {
        if content.is_empty() {
            return;
        }

        let state = self.state_mut(kind);
        state.has_seen_delta = true;
        state.collector.push_delta(&content);

        if content.contains('\n') {
            let committed = state.collector.commit_complete_lines();
            if !committed.is_empty() {
                state.enqueue(committed);
            }
        }
    }

    pub fn drain_commit_tick(&mut self) -> DrainedLines {
        self.drain_commit_tick_at(Instant::now())
    }

    pub fn flush_pending(&mut self) -> DrainedLines {
        self.flush_pending_at()
    }

    pub fn tail_text(&self, kind: StreamKind) -> String {
        self.state(kind).collector.current_tail().to_string()
    }

    /// Returns true if any deltas were pushed to the streaming controller
    /// since the last clear.
    pub fn has_seen_any_delta(&self) -> bool {
        self.text_state.has_seen_delta || self.thinking_state.has_seen_delta
    }

    /// Finalize and drain a single stream kind (e.g. when switching from
    /// thinking to text). Returns the flushed lines for that kind only.
    pub fn flush_kind(&mut self, kind: StreamKind) -> Vec<Line<'static>> {
        let state = self.state_mut(kind);
        let remaining = state.collector.finalize_and_drain();
        if !remaining.is_empty() {
            state.enqueue(remaining);
        }
        state.drain_all()
    }

    fn drain_commit_tick_at(&mut self, now: Instant) -> DrainedLines {
        let output = run_commit_tick(
            &mut self.policy,
            &mut self.text_state,
            &mut self.thinking_state,
            now,
        );
        Self::to_drained_lines(output)
    }

    fn flush_pending_at(&mut self) -> DrainedLines {
        let text_remaining = self.text_state.collector.finalize_and_drain();
        if !text_remaining.is_empty() {
            self.text_state.enqueue(text_remaining);
        }
        let thinking_remaining = self.thinking_state.collector.finalize_and_drain();
        if !thinking_remaining.is_empty() {
            self.thinking_state.enqueue(thinking_remaining);
        }

        self.policy.reset();
        DrainedLines {
            text: self.text_state.drain_all(),
            thinking: self.thinking_state.drain_all(),
        }
    }

    fn to_drained_lines(output: CommitTickOutput) -> DrainedLines {
        DrainedLines {
            text: output.text_lines,
            thinking: output.thinking_lines,
        }
    }

    fn state(&self, kind: StreamKind) -> &StreamState {
        match kind {
            StreamKind::Text => &self.text_state,
            StreamKind::Thinking => &self.thinking_state,
        }
    }

    fn state_mut(&mut self, kind: StreamKind) -> &mut StreamState {
        match kind {
            StreamKind::Text => &mut self.text_state,
            StreamKind::Thinking => &mut self.thinking_state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newline_gating_commits_only_complete_lines() {
        let mut controller = StreamingController::new();
        controller.push(StreamKind::Text, "hello".to_string());
        let drained = controller.drain_commit_tick();
        assert!(drained.text.is_empty());

        controller.push(StreamKind::Text, " world\nnext".to_string());
        let drained = controller.drain_commit_tick();
        assert_eq!(drained.text.len(), 1);
        assert_eq!(controller.tail_text(StreamKind::Text), "next");
    }

    #[test]
    fn flush_pending_drains_queue_and_partials() {
        let mut controller = StreamingController::new();
        controller.push(StreamKind::Text, "line\n".to_string());
        controller.push(StreamKind::Text, "tail".to_string());

        let drained = controller.flush_pending();
        assert_eq!(drained.text.len(), 2);
        assert!(controller.tail_text(StreamKind::Text).is_empty());
    }

    #[test]
    fn identical_consecutive_deltas_are_preserved() {
        let mut controller = StreamingController::new();
        controller.push(StreamKind::Text, "dup line\n".to_string());
        controller.push(StreamKind::Text, "dup line\n".to_string());

        let drained = controller.flush_pending();
        assert_eq!(drained.text.len(), 2);
    }
}
