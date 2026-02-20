use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkingMode {
    Smooth,
    CatchUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainPlan {
    Single,
    Batch(usize),
}

#[derive(Debug, Clone, Copy)]
pub struct QueueSnapshot {
    pub queued_lines: usize,
    pub oldest_age: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkingDecision {
    pub mode: ChunkingMode,
    pub drain_plan: DrainPlan,
    pub entered_catch_up: bool,
}

pub struct AdaptiveChunkingPolicy {
    mode: ChunkingMode,
    catch_up_below_threshold_since: Option<Instant>,
}

impl AdaptiveChunkingPolicy {
    const ENTER_LINES: usize = 8;
    const ENTER_AGE: Duration = Duration::from_millis(120);
    const EXIT_LINES: usize = 2;
    const EXIT_AGE: Duration = Duration::from_millis(40);
    const EXIT_HOLD: Duration = Duration::from_millis(250);

    pub fn new() -> Self {
        Self {
            mode: ChunkingMode::Smooth,
            catch_up_below_threshold_since: None,
        }
    }

    #[cfg(test)]
    pub fn mode(&self) -> ChunkingMode {
        self.mode
    }

    pub fn reset(&mut self) {
        self.mode = ChunkingMode::Smooth;
        self.catch_up_below_threshold_since = None;
    }

    pub fn decide(&mut self, snapshot: QueueSnapshot, now: Instant) -> ChunkingDecision {
        if snapshot.queued_lines == 0 {
            self.reset();
            return ChunkingDecision {
                mode: self.mode,
                drain_plan: DrainPlan::Single,
                entered_catch_up: false,
            };
        }

        let enter_catch_up = snapshot.queued_lines >= Self::ENTER_LINES
            || snapshot
                .oldest_age
                .is_some_and(|age| age >= Self::ENTER_AGE);

        let exit_candidate = snapshot.queued_lines <= Self::EXIT_LINES
            && snapshot.oldest_age.is_some_and(|age| age <= Self::EXIT_AGE);

        let prior_mode = self.mode;
        match self.mode {
            ChunkingMode::Smooth => {
                if enter_catch_up {
                    self.mode = ChunkingMode::CatchUp;
                    self.catch_up_below_threshold_since = None;
                }
            }
            ChunkingMode::CatchUp => {
                if exit_candidate {
                    let since = self.catch_up_below_threshold_since.get_or_insert(now);
                    if now.saturating_duration_since(*since) >= Self::EXIT_HOLD {
                        self.mode = ChunkingMode::Smooth;
                        self.catch_up_below_threshold_since = None;
                    }
                } else {
                    self.catch_up_below_threshold_since = None;
                }
            }
        }

        let drain_plan = match self.mode {
            ChunkingMode::Smooth => DrainPlan::Single,
            ChunkingMode::CatchUp => DrainPlan::Batch(snapshot.queued_lines),
        };

        ChunkingDecision {
            mode: self.mode,
            drain_plan,
            entered_catch_up: prior_mode != ChunkingMode::CatchUp
                && self.mode == ChunkingMode::CatchUp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enters_catch_up_when_queue_grows() {
        let mut policy = AdaptiveChunkingPolicy::new();
        let now = Instant::now();
        let decision = policy.decide(
            QueueSnapshot {
                queued_lines: 8,
                oldest_age: Some(Duration::from_millis(10)),
            },
            now,
        );
        assert_eq!(decision.mode, ChunkingMode::CatchUp);
        assert_eq!(decision.drain_plan, DrainPlan::Batch(8));
    }

    #[test]
    fn exits_catch_up_after_hold_window() {
        let mut policy = AdaptiveChunkingPolicy::new();
        let t0 = Instant::now();
        let _ = policy.decide(
            QueueSnapshot {
                queued_lines: 9,
                oldest_age: Some(Duration::from_millis(10)),
            },
            t0,
        );
        assert_eq!(policy.mode(), ChunkingMode::CatchUp);

        let _ = policy.decide(
            QueueSnapshot {
                queued_lines: 1,
                oldest_age: Some(Duration::from_millis(10)),
            },
            t0 + Duration::from_millis(50),
        );
        assert_eq!(policy.mode(), ChunkingMode::CatchUp);

        let _ = policy.decide(
            QueueSnapshot {
                queued_lines: 1,
                oldest_age: Some(Duration::from_millis(10)),
            },
            t0 + Duration::from_millis(350),
        );
        assert_eq!(policy.mode(), ChunkingMode::Smooth);
    }
}
