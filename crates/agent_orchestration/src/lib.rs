//! Storage-agnostic orchestration domain shared by code-assistant hosts and
//! downstream consumers (PAL is the first): durable goals with a
//! deterministic controller policy, and the typed wait barriers a goal can
//! park on.
//!
//! See ROADMAP.md ("Crate direction" / "Now: converge goals and exact turn
//! ownership"): this crate owns the *generic semantics* — entities, state
//! machines, the attempt/evidence ledger, claim/revision atomicity, and the
//! evaluator seam. Hosts own everything whose meaning comes from deployment:
//! which session incarnation pursues a goal, startup sweeps and orphan
//! adoption, durable timers/jobs/children/event probes, channel delivery.
//!
//! It deliberately depends on no frontend, no channels, and no global config
//! paths. The bundled [`goals::GoalStore`]/[`waits::WaitStore`] are plain
//! JSON-file stores rooted at an explicit path; the [`goals::GoalRepository`]
//! and [`waits::WaitRepository`] traits are the seam a transactional
//! repository implements instead.

pub mod goal_eval;
pub mod goals;
pub mod runs;
pub mod waits;

use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable identity of the owner a goal or wait is bound to. Hosts define the
/// shape: PAL uses conversation-lane keys such as `telegram:private:42`,
/// an interactive code-assistant host can use a session or project id. The
/// orchestration domain only compares and stores it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OwnerKey(String);

impl OwnerKey {
    /// Build a key from its hierarchy of parts (channel, chat, …).
    pub fn from_parts(parts: &[&str]) -> Self {
        Self(parts.join(":"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OwnerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
