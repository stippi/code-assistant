use std::collections::VecDeque;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkingMode {
    Smooth,
    CatchUp,
}

pub struct StreamingController {
    queue: VecDeque<StreamDelta>,
    mode: ChunkingMode,
}

impl StreamingController {
    const ENTER_CATCH_UP_AT: usize = 8;
    const EXIT_CATCH_UP_AT: usize = 2;

    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            mode: ChunkingMode::Smooth,
        }
    }

    pub fn clear(&mut self) {
        self.queue.clear();
        self.mode = ChunkingMode::Smooth;
    }

    pub fn push(&mut self, kind: StreamKind, content: String) {
        if content.is_empty() {
            return;
        }
        self.queue.push_back(StreamDelta { kind, content });
        self.update_mode();
    }

    pub fn drain_commit_tick(&mut self) -> Vec<StreamDelta> {
        self.update_mode();
        let drain_count = match self.mode {
            ChunkingMode::Smooth => 1,
            ChunkingMode::CatchUp => self.queue.len(),
        };

        let mut drained = Vec::with_capacity(drain_count);
        for _ in 0..drain_count {
            let Some(next) = self.queue.pop_front() else {
                break;
            };
            drained.push(next);
        }
        self.update_mode();
        drained
    }

    fn update_mode(&mut self) {
        let queued = self.queue.len();
        self.mode = match self.mode {
            ChunkingMode::Smooth if queued >= Self::ENTER_CATCH_UP_AT => ChunkingMode::CatchUp,
            ChunkingMode::CatchUp if queued <= Self::EXIT_CATCH_UP_AT => ChunkingMode::Smooth,
            mode => mode,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_mode_drains_single_delta() {
        let mut controller = StreamingController::new();
        controller.push(StreamKind::Text, "a".to_string());
        controller.push(StreamKind::Text, "b".to_string());

        let first = controller.drain_commit_tick();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].content, "a");

        let second = controller.drain_commit_tick();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].content, "b");
    }

    #[test]
    fn catch_up_mode_drains_full_queue() {
        let mut controller = StreamingController::new();
        for i in 0..10 {
            controller.push(StreamKind::Text, i.to_string());
        }

        let drained = controller.drain_commit_tick();
        assert_eq!(drained.len(), 10);
        assert!(controller.drain_commit_tick().is_empty());
    }
}
