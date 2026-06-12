use std::collections::VecDeque;
use std::time::Duration;
use std::time::Instant;

use ratatui::text::Line;

use self::markdown_stream::MarkdownStreamCollector;

pub mod chunking;
pub mod commit_tick;
pub mod controller;
pub mod markdown_stream;

pub struct QueuedLine {
    pub line: Line<'static>,
    pub enqueued_at: Instant,
}

/// Holds in-flight markdown stream state and queued committed lines.
pub struct StreamState {
    pub collector: MarkdownStreamCollector,
    queued_lines: VecDeque<QueuedLine>,
    pub has_seen_delta: bool,
}

impl StreamState {
    pub fn new(width: Option<usize>) -> Self {
        Self {
            collector: MarkdownStreamCollector::new(width),
            queued_lines: VecDeque::new(),
            has_seen_delta: false,
        }
    }

    pub fn set_width(&mut self, width: Option<usize>) {
        self.collector.set_width(width);
    }

    pub fn clear(&mut self) {
        self.collector.clear();
        self.queued_lines.clear();
        self.has_seen_delta = false;
    }

    pub fn drain_n(&mut self, max_lines: usize) -> Vec<Line<'static>> {
        let end = max_lines.min(self.queued_lines.len());
        self.queued_lines
            .drain(..end)
            .map(|queued| queued.line)
            .collect()
    }

    pub fn drain_all(&mut self) -> Vec<Line<'static>> {
        self.queued_lines
            .drain(..)
            .map(|queued| queued.line)
            .collect()
    }

    pub fn queued_len(&self) -> usize {
        self.queued_lines.len()
    }

    pub fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        self.queued_lines
            .front()
            .map(|queued| now.saturating_duration_since(queued.enqueued_at))
    }

    pub fn enqueue(&mut self, lines: Vec<Line<'static>>) {
        let now = Instant::now();
        self.queued_lines
            .extend(lines.into_iter().map(|line| QueuedLine {
                line,
                enqueued_at: now,
            }));
    }
}
