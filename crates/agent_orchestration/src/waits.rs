//! Durable wait conditions: a goal parks on an external barrier instead of
//! burning model turns polling for it. `schedule_wakeup` and a PTY session id
//! are useful *inside* a live incarnation, but neither survives rotation or a
//! restart — they are not durable dependency edges. A [`Wait`] is the pal_core
//! entity that turns "poll again in 20 minutes" into "finish this when CI /
//! the deployment / the requested document is ready": it is persisted, it wakes
//! exactly the one goal that owns it, and while it is armed the goal spends no
//! model turns at all (a `Waiting` goal is never driven by the controller —
//! see [`crate::goals`]).
//!
//! This module is the *domain* layer only: the barrier taxonomy
//! ([`WaitKind`]), the wait state machine and the clock-only predicates
//! (`timed_out`, [`WaitKind::due`]) that resolve a wait without any I/O.
//! Whether a *runtime-observable* barrier (a process exit, a job completion, an
//! external event) has fired is decided by an injected probe at the gateway
//! layer, so this policy stays testable without real processes. Persistence is
//! [`WaitStore`], the sweep that folds a probe outcome into a resolution and
//! wakes the owning goal lives in the host (PAL's gateway `wait_pass`).
//!
//! All timestamps are naive local time, like [`crate::goals`] and
//! the host's job scheduler.

use crate::OwnerKey;
use chrono::NaiveDateTime;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// What a wait is blocked on. The domain layer treats each barrier's
/// satisfaction opaquely: `Until` resolves against the clock alone,
/// `HumanInput` against a user message on the owner, and every other kind
/// against an injected runtime probe. The variant carries only the *identity*
/// of the thing waited on — never a live handle — so a wait means the same
/// after a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "barrier", rename_all = "snake_case")]
pub enum WaitKind {
    /// A pure wall-clock barrier: fires the moment `now >= at`. A durable,
    /// turn-free sleep — distinct from a *timeout*, which is a wait's failure
    /// edge rather than its success edge.
    Until { at: NaiveDateTime },
    /// A background process (identified by a durable PTY/session handle) has
    /// exited, whatever its exit status. The status rides along in the
    /// resolution note so the woken goal can react to it.
    ProcessExit { handle: String },
    /// A background process' accumulated output matched `pattern` (e.g. a build
    /// printed `BUILD SUCCESSFUL`). Fires on first match.
    OutputPattern { handle: String, pattern: String },
    /// A durable job's run reached a terminal outcome.
    JobCompletion { job_id: String },
    /// A spawned child agent finished its run.
    SubAgentCompletion { agent_id: String },
    /// An external event carrying this dedupe key was signalled (the seam the
    /// later event-source work feeds).
    Event { key: String },
    /// The user answered on the goal's own owner — the human-input barrier that
    /// the gateway satisfies from the same path that preempts running goals.
    HumanInput,
}

impl WaitKind {
    /// Whether a `Until` barrier has been reached at `now`. Always `false` for
    /// every other kind — their satisfaction is not a function of the clock.
    pub fn due(&self, now: NaiveDateTime) -> bool {
        matches!(self, WaitKind::Until { at } if now >= *at)
    }

    /// Whether this barrier resolves against the clock alone, needing no
    /// runtime probe and no external signal. Only `Until` does.
    pub fn is_clock_only(&self) -> bool {
        matches!(self, WaitKind::Until { .. })
    }

    /// Whether this barrier is satisfied by a user message on the owner rather
    /// than by a probe. Only `HumanInput` is.
    pub fn is_human_input(&self) -> bool {
        matches!(self, WaitKind::HumanInput)
    }

    /// A stable lowercase label for prompts and listings.
    pub fn label(&self) -> &'static str {
        match self {
            WaitKind::Until { .. } => "until",
            WaitKind::ProcessExit { .. } => "process_exit",
            WaitKind::OutputPattern { .. } => "output_pattern",
            WaitKind::JobCompletion { .. } => "job_completion",
            WaitKind::SubAgentCompletion { .. } => "sub_agent_completion",
            WaitKind::Event { .. } => "event",
            WaitKind::HumanInput => "human_input",
        }
    }
}

/// A request to arm a durable wait, produced by a goal evaluation (the
/// [`crate::goals::AttemptVerdict::Waiting`] verdict) and turned into a
/// persisted [`Wait`] by the gateway through [`WaitStore::arm`]. Carries only
/// the barrier and its optional timeout — the id, owning goal and owner are
/// supplied at arming time, so the same request can be judged, logged and
/// stored without knowing those yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitRequest {
    pub kind: WaitKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<NaiveDateTime>,
}

impl WaitRequest {
    pub fn new(kind: WaitKind) -> Self {
        Self {
            kind,
            timeout: None,
        }
    }

    pub fn with_timeout(mut self, deadline: NaiveDateTime) -> Self {
        self.timeout = Some(deadline);
        self
    }
}

/// Where a wait is in its lifecycle. `Armed` is the only live state; the three
/// resolutions are terminal and mutually exclusive.
///
/// - `Satisfied`: the barrier fired — the goal wakes to make use of it.
/// - `TimedOut`: the optional wall-clock timeout elapsed first — the goal wakes
///   to react to the *absence* of what it waited for.
/// - `Cancelled`: the wait was retired without firing (its goal was paused,
///   cancelled or itself reached a terminal state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitState {
    Armed,
    Satisfied,
    TimedOut,
    Cancelled,
}

impl WaitState {
    /// A resolution admits no further transitions.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, WaitState::Armed)
    }

    /// Whether the owning goal should be woken when the wait reaches this
    /// state. Both a fired barrier and an elapsed timeout wake the goal (it
    /// must react either way); a cancelled wait does not (its goal was already
    /// moved on by whoever cancelled it).
    pub fn wakes_goal(&self) -> bool {
        matches!(self, WaitState::Satisfied | WaitState::TimedOut)
    }

    pub fn label(&self) -> &'static str {
        match self {
            WaitState::Armed => "armed",
            WaitState::Satisfied => "satisfied",
            WaitState::TimedOut => "timed_out",
            WaitState::Cancelled => "cancelled",
        }
    }
}

/// A durable barrier owned by exactly one goal. While it is `Armed` the goal is
/// `Waiting` and consumes no turns; a resolution is the signal to wake that one
/// goal (or, for `Cancelled`, that the goal was already moved on).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wait {
    pub id: String,
    /// Optimistic-concurrency revision, mirroring [`crate::goals::Goal`]. A
    /// stale sweep snapshot cannot overwrite a newer cancellation.
    #[serde(default)]
    pub revision: u64,
    /// The goal this wait wakes. Exactly one — a wait is never shared.
    pub goal_id: String,
    /// The goal's owner, denormalised so the human-input path can find the wait
    /// by owner without loading the goal store.
    #[serde(alias = "lane")]
    pub owner: OwnerKey,
    pub kind: WaitKind,
    pub state: WaitState,
    /// Optional wall-clock timeout. Reaching it while `Armed` resolves the wait
    /// to `TimedOut`. Independent of `WaitKind::Until`, whose `at` is the
    /// *success* barrier — an `Until` wait may also carry a (later) timeout,
    /// though that is rarely useful.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<NaiveDateTime>,
    /// Why the wait resolved the way it did (a process exit status, the matched
    /// line, a timeout notice, a cancellation reason). Carried into the goal's
    /// wake note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub armed_at: NaiveDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<NaiveDateTime>,
    pub updated_at: NaiveDateTime,
}

impl Wait {
    pub fn new(
        id: impl Into<String>,
        goal_id: impl Into<String>,
        owner: OwnerKey,
        kind: WaitKind,
        timeout: Option<NaiveDateTime>,
        now: NaiveDateTime,
    ) -> Self {
        Self {
            id: id.into(),
            revision: 0,
            goal_id: goal_id.into(),
            owner,
            kind,
            state: WaitState::Armed,
            timeout,
            note: None,
            armed_at: now,
            resolved_at: None,
            updated_at: now,
        }
    }

    /// Whether the wait is still live (waiting for its barrier).
    pub fn is_active(&self) -> bool {
        self.state == WaitState::Armed
    }

    /// Whether the optional timeout has elapsed at `now`. `false` when there is
    /// no timeout — a wait with no timeout waits forever (until cancelled).
    pub fn timed_out(&self, now: NaiveDateTime) -> bool {
        self.timeout.is_some_and(|deadline| now >= deadline)
    }

    /// Whether the wait's *own* clock-only barrier is due — i.e. it is an
    /// `Until` wait whose target time has been reached. Timeout is separate
    /// (see [`Wait::timed_out`]); this is the success edge.
    pub fn due(&self, now: NaiveDateTime) -> bool {
        self.kind.due(now)
    }

    /// The barrier fired. `Armed -> Satisfied`, carrying an optional note (a
    /// process exit status, the matched output line, …). Errors if already
    /// resolved so a double-fire cannot rewrite an outcome.
    pub fn satisfy(&mut self, note: Option<String>, now: NaiveDateTime) -> anyhow::Result<()> {
        self.resolve(WaitState::Satisfied, note, now)
    }

    /// The timeout elapsed before the barrier fired. `Armed -> TimedOut`.
    pub fn time_out(&mut self, now: NaiveDateTime) -> anyhow::Result<()> {
        self.resolve(
            WaitState::TimedOut,
            Some(format!("wait for {} timed out", self.kind.label())),
            now,
        )
    }

    /// The wait is retired without firing (its goal was paused/cancelled/failed
    /// or superseded). `Armed -> Cancelled`. Idempotent-friendly: cancelling an
    /// already-resolved wait is a no-op that returns `false`.
    pub fn cancel(&mut self, reason: impl Into<String>, now: NaiveDateTime) -> bool {
        if self.state.is_terminal() {
            return false;
        }
        let _ = self.resolve(WaitState::Cancelled, Some(reason.into()), now);
        true
    }

    fn resolve(
        &mut self,
        state: WaitState,
        note: Option<String>,
        now: NaiveDateTime,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.state == WaitState::Armed,
            "cannot resolve a {} wait to {}",
            self.state.label(),
            state.label()
        );
        self.state = state;
        self.note = note;
        self.resolved_at = Some(now);
        self.updated_at = now;
        Ok(())
    }

    /// A short human/prompt line describing what this wait is blocked on.
    pub fn describe(&self) -> String {
        let barrier = match &self.kind {
            WaitKind::Until { at } => format!("until {}", at.format("%Y-%m-%d %H:%M")),
            WaitKind::ProcessExit { handle } => format!("process {handle} to exit"),
            WaitKind::OutputPattern { handle, pattern } => {
                format!("process {handle} to print /{pattern}/")
            }
            WaitKind::JobCompletion { job_id } => format!("job {job_id} to finish"),
            WaitKind::SubAgentCompletion { agent_id } => format!("agent {agent_id} to finish"),
            WaitKind::Event { key } => format!("event {key}"),
            WaitKind::HumanInput => "a reply from you".to_string(),
        };
        match self.timeout {
            Some(deadline) => {
                format!(
                    "waiting for {barrier} (timeout {})",
                    deadline.format("%Y-%m-%d %H:%M")
                )
            }
            None => format!("waiting for {barrier}"),
        }
    }
}

/// The result of polling a runtime-observable barrier. `Pending` leaves the
/// wait armed for the next sweep; `Fired` resolves it (and wakes the owning
/// goal), carrying an optional note that becomes the goal's wake reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitProbeOutcome {
    Pending,
    Fired { note: Option<String> },
}

impl WaitProbeOutcome {
    /// A fired barrier with no extra detail.
    pub fn fired() -> Self {
        WaitProbeOutcome::Fired { note: None }
    }

    /// A fired barrier carrying a note (an exit status, the matched line, …).
    pub fn fired_with(note: impl Into<String>) -> Self {
        WaitProbeOutcome::Fired {
            note: Some(note.into()),
        }
    }
}

/// The runtime-facing seam that decides whether a *runtime-observable* barrier
/// (a process exit, an output match, a job or child-agent completion, an
/// external event) has fired. The clock-only `Until` barrier and the
/// `HumanInput` barrier are resolved by the gateway without ever reaching a
/// probe, so an implementation only needs to handle the observable kinds; it
/// may return `Pending` for anything it does not recognise. Injected exactly
/// like [`crate::goals::GoalEvaluator`], which keeps the sweep policy testable
/// without real processes.
#[async_trait::async_trait]
pub trait WaitProbe: Send + Sync {
    async fn poll(&self, wait: &Wait) -> anyhow::Result<WaitProbeOutcome>;
}

/// JSON-file persistence for durable waits (`waits.json`). Mirrors
/// [`crate::goals::GoalStore`]: every operation reloads the current file and
/// writes through atomic tmp+rename, instances for the same path share a
/// process-local mutex, an OS advisory lock serializes transactions across
/// processes, and [`WaitStore::update`] rejects stale revisions.
///
/// The store holds only *live* (`Armed`) waits. Resolving a wait is the job of
/// the gateway sweep, which wakes the owning goal and then [`WaitStore::remove`]s
/// the wait — so a resolution is never persisted here. The `Satisfied` /
/// `TimedOut` / `Cancelled` states exist for the in-memory transition and for
/// the crash-window reconciliation the sweep performs (a still-`Armed` wait
/// whose goal is no longer `Waiting` is stale and dropped). At most one wait is
/// armed per goal: [`WaitStore::arm`] supersedes any earlier barrier, which
/// keeps waking unambiguous.
pub struct WaitStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl WaitStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let lock = wait_store_lock(&path);
        Self { path, lock }
    }

    fn transaction(&self) -> anyhow::Result<(MutexGuard<'_, ()>, File)> {
        let process_guard = self.lock.lock().expect("wait store lock poisoned");
        let file_guard = lock_store_file(&self.path)?;
        Ok((process_guard, file_guard))
    }

    /// All persisted waits (normally all `Armed`); a missing file is empty.
    pub fn list(&self) -> anyhow::Result<Vec<Wait>> {
        let (_process_guard, _file_guard) = self.transaction()?;
        self.load_unlocked()
    }

    fn load_unlocked(&self) -> anyhow::Result<Vec<Wait>> {
        match std::fs::read_to_string(&self.path) {
            Ok(content) => Ok(serde_json::from_str(&content)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn save_unlocked(&self, waits: &[Wait]) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(waits)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Add a wait with a caller-supplied id; the id must be unique. Prefer
    /// [`WaitStore::arm`], which also supersedes a goal's earlier barrier.
    pub fn add(&self, wait: Wait) -> anyhow::Result<()> {
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut waits = self.load_unlocked()?;
        if waits.iter().any(|w| w.id == wait.id) {
            anyhow::bail!("wait id {} already exists", wait.id);
        }
        waits.push(wait);
        self.save_unlocked(&waits)
    }

    /// Arm a durable barrier for a goal, superseding any wait it already owns
    /// (a goal parks on exactly one barrier at a time). The store assigns the
    /// id, returned for display and cancellation.
    pub fn arm(
        &self,
        goal_id: impl Into<String>,
        owner: OwnerKey,
        kind: WaitKind,
        timeout: Option<NaiveDateTime>,
        now: NaiveDateTime,
    ) -> anyhow::Result<Wait> {
        let goal_id = goal_id.into();
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut waits = self.load_unlocked()?;
        // Supersede: a goal never holds two armed barriers.
        waits.retain(|w| w.goal_id != goal_id);
        let base = format!("wait-{}", now.format("%Y%m%d-%H%M%S"));
        let mut id = base.clone();
        let mut n = 1;
        while waits.iter().any(|w| w.id == id) {
            n += 1;
            id = format!("{base}-{n}");
        }
        let wait = Wait::new(id, goal_id, owner, kind, timeout, now);
        waits.push(wait.clone());
        self.save_unlocked(&waits)?;
        Ok(wait)
    }

    /// A single wait by id.
    pub fn get(&self, id: &str) -> anyhow::Result<Option<Wait>> {
        Ok(self.list()?.into_iter().find(|w| w.id == id))
    }

    /// Persist a mutated wait against the revision it was loaded at. Errors on
    /// an unknown id (an update never silently creates) or a revision conflict.
    pub fn update(&self, wait: &Wait) -> anyhow::Result<Wait> {
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut waits = self.load_unlocked()?;
        let Some(slot) = waits.iter_mut().find(|w| w.id == wait.id) else {
            anyhow::bail!("unknown wait id {}", wait.id);
        };
        anyhow::ensure!(
            slot.revision == wait.revision,
            "wait {} revision conflict: expected {}, found {}",
            wait.id,
            wait.revision,
            slot.revision
        );
        let mut persisted = wait.clone();
        persisted.revision = persisted.revision.saturating_add(1);
        *slot = persisted.clone();
        self.save_unlocked(&waits)?;
        Ok(persisted)
    }

    /// Remove a wait; `false` when the id is unknown. A resolved wait is removed
    /// (not persisted terminal) — the goal's wake note carries the outcome.
    pub fn remove(&self, id: &str) -> anyhow::Result<bool> {
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut waits = self.load_unlocked()?;
        let before = waits.len();
        waits.retain(|w| w.id != id);
        let removed = waits.len() != before;
        if removed {
            self.save_unlocked(&waits)?;
        }
        Ok(removed)
    }

    /// Every live wait — the set the sweep must probe.
    pub fn armed(&self) -> anyhow::Result<Vec<Wait>> {
        Ok(self.list()?.into_iter().filter(Wait::is_active).collect())
    }

    /// Live waits owned by one goal (normally at most one — see
    /// [`WaitStore::arm`]).
    pub fn armed_for_goal(&self, goal_id: &str) -> anyhow::Result<Vec<Wait>> {
        Ok(self
            .armed()?
            .into_iter()
            .filter(|w| w.goal_id == goal_id)
            .collect())
    }

    /// Live waits armed on one owner.
    pub fn armed_for_owner(&self, owner: &OwnerKey) -> anyhow::Result<Vec<Wait>> {
        Ok(self
            .armed()?
            .into_iter()
            .filter(|w| &w.owner == owner)
            .collect())
    }

    /// Retire every wait a goal owns because the goal itself moved on (paused,
    /// cancelled, blocked or reached a terminal state). Returns the removed
    /// ids. Since a resolved wait is removed anyway, cancellation is a removal —
    /// the reason is not persisted, only logged by the caller.
    pub fn cancel_for_goal(&self, goal_id: &str) -> anyhow::Result<Vec<String>> {
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut waits = self.load_unlocked()?;
        let mut cancelled = Vec::new();
        waits.retain(|w| {
            if w.goal_id == goal_id {
                cancelled.push(w.id.clone());
                false
            } else {
                true
            }
        });
        if !cancelled.is_empty() {
            self.save_unlocked(&waits)?;
        }
        Ok(cancelled)
    }

    /// A user message arrived on `owner`: satisfy and remove every armed
    /// `HumanInput` wait there, returning them so the caller can wake their
    /// goals. The human-input barrier is resolved from the same path that
    /// preempts running goals, never by the runtime probe.
    pub fn take_human_input_for_owner(
        &self,
        owner: &OwnerKey,
        now: NaiveDateTime,
    ) -> anyhow::Result<Vec<Wait>> {
        let (_process_guard, _file_guard) = self.transaction()?;
        let mut waits = self.load_unlocked()?;
        let mut taken = Vec::new();
        waits.retain(|w| {
            if &w.owner == owner && w.is_active() && w.kind.is_human_input() {
                let mut w = w.clone();
                let _ = w.satisfy(Some("you replied".into()), now);
                taken.push(w);
                false
            } else {
                true
            }
        });
        if !taken.is_empty() {
            self.save_unlocked(&waits)?;
        }
        Ok(taken)
    }
}

/// Storage seam for waits, mirroring [`crate::goals::GoalRepository`]: the
/// bundled JSON [`WaitStore`] implements it, a transactional repository can
/// replace it without touching sweep or controller code.
pub trait WaitRepository: Send + Sync {
    fn list(&self) -> anyhow::Result<Vec<Wait>>;
    fn add(&self, wait: Wait) -> anyhow::Result<()>;
    fn arm(
        &self,
        goal_id: String,
        owner: OwnerKey,
        kind: WaitKind,
        timeout: Option<NaiveDateTime>,
        now: NaiveDateTime,
    ) -> anyhow::Result<Wait>;
    fn get(&self, id: &str) -> anyhow::Result<Option<Wait>>;
    fn update(&self, wait: &Wait) -> anyhow::Result<Wait>;
    fn remove(&self, id: &str) -> anyhow::Result<bool>;
    fn armed(&self) -> anyhow::Result<Vec<Wait>>;
    fn armed_for_goal(&self, goal_id: &str) -> anyhow::Result<Vec<Wait>>;
    fn armed_for_owner(&self, owner: &OwnerKey) -> anyhow::Result<Vec<Wait>>;
    fn cancel_for_goal(&self, goal_id: &str) -> anyhow::Result<Vec<String>>;
    fn take_human_input_for_owner(
        &self,
        owner: &OwnerKey,
        now: NaiveDateTime,
    ) -> anyhow::Result<Vec<Wait>>;
}

impl WaitRepository for WaitStore {
    fn list(&self) -> anyhow::Result<Vec<Wait>> {
        WaitStore::list(self)
    }
    fn add(&self, wait: Wait) -> anyhow::Result<()> {
        WaitStore::add(self, wait)
    }
    fn arm(
        &self,
        goal_id: String,
        owner: OwnerKey,
        kind: WaitKind,
        timeout: Option<NaiveDateTime>,
        now: NaiveDateTime,
    ) -> anyhow::Result<Wait> {
        WaitStore::arm(self, goal_id, owner, kind, timeout, now)
    }
    fn get(&self, id: &str) -> anyhow::Result<Option<Wait>> {
        WaitStore::get(self, id)
    }
    fn update(&self, wait: &Wait) -> anyhow::Result<Wait> {
        WaitStore::update(self, wait)
    }
    fn remove(&self, id: &str) -> anyhow::Result<bool> {
        WaitStore::remove(self, id)
    }
    fn armed(&self) -> anyhow::Result<Vec<Wait>> {
        WaitStore::armed(self)
    }
    fn armed_for_goal(&self, goal_id: &str) -> anyhow::Result<Vec<Wait>> {
        WaitStore::armed_for_goal(self, goal_id)
    }
    fn armed_for_owner(&self, owner: &OwnerKey) -> anyhow::Result<Vec<Wait>> {
        WaitStore::armed_for_owner(self, owner)
    }
    fn cancel_for_goal(&self, goal_id: &str) -> anyhow::Result<Vec<String>> {
        WaitStore::cancel_for_goal(self, goal_id)
    }
    fn take_human_input_for_owner(
        &self,
        owner: &OwnerKey,
        now: NaiveDateTime,
    ) -> anyhow::Result<Vec<Wait>> {
        WaitStore::take_human_input_for_owner(self, owner, now)
    }
}

fn wait_store_lock(path: &std::path::Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks.lock().expect("wait store lock registry poisoned");
    locks
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn lock_store_file(path: &Path) -> anyhow::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("json.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    FileExt::lock_exclusive(&file)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    /// PAL's existing waits.json predates the OwnerKey generalization and
    /// keys the owner as "lane" — the serde alias must keep those files
    /// readable.
    #[test]
    fn legacy_lane_keyed_json_still_deserializes() {
        let wait = Wait::new(
            "w-legacy",
            "g1",
            owner(),
            WaitKind::HumanInput,
            None,
            at(2026, 7, 15, 8, 0),
        );
        let json = serde_json::to_string(&wait)
            .unwrap()
            .replace("\"owner\"", "\"lane\"");
        let restored: Wait = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.owner, wait.owner);
    }

    fn owner() -> OwnerKey {
        OwnerKey::from_parts(&["telegram", "private", "42"])
    }

    fn wait(kind: WaitKind, timeout: Option<NaiveDateTime>) -> Wait {
        Wait::new(
            "wait-1",
            "goal-1",
            owner(),
            kind,
            timeout,
            at(2026, 7, 14, 9, 0),
        )
    }

    #[test]
    fn new_wait_is_armed_and_unresolved() {
        let w = wait(
            WaitKind::ProcessExit {
                handle: "pty-7".into(),
            },
            None,
        );
        assert_eq!(w.state, WaitState::Armed);
        assert!(w.is_active());
        assert_eq!(w.goal_id, "goal-1");
        assert!(w.note.is_none());
        assert!(w.resolved_at.is_none());
        assert_eq!(w.armed_at, w.updated_at);
    }

    #[test]
    fn armed_is_the_only_non_terminal_state() {
        assert!(!WaitState::Armed.is_terminal());
        assert!(WaitState::Satisfied.is_terminal());
        assert!(WaitState::TimedOut.is_terminal());
        assert!(WaitState::Cancelled.is_terminal());
    }

    #[test]
    fn only_a_fired_or_timed_out_wait_wakes_the_goal() {
        assert!(WaitState::Satisfied.wakes_goal());
        assert!(WaitState::TimedOut.wakes_goal());
        assert!(!WaitState::Cancelled.wakes_goal());
        assert!(!WaitState::Armed.wakes_goal());
    }

    #[test]
    fn until_is_the_only_clock_only_barrier() {
        let until = WaitKind::Until {
            at: at(2026, 7, 14, 12, 0),
        };
        assert!(until.is_clock_only());
        assert!(!until.is_human_input());
        assert!(WaitKind::HumanInput.is_human_input());
        assert!(!WaitKind::HumanInput.is_clock_only());
        assert!(!WaitKind::ProcessExit { handle: "x".into() }.is_clock_only());
    }

    #[test]
    fn until_is_due_only_once_its_time_is_reached() {
        let target = at(2026, 7, 14, 12, 0);
        let w = wait(WaitKind::Until { at: target }, None);
        assert!(!w.due(at(2026, 7, 14, 11, 59)));
        assert!(w.due(target));
        assert!(w.due(at(2026, 7, 14, 12, 1)));
        // A non-Until wait is never "due" on the clock.
        let p = wait(WaitKind::ProcessExit { handle: "x".into() }, None);
        assert!(!p.due(at(2999, 1, 1, 0, 0)));
    }

    #[test]
    fn timeout_is_independent_of_the_until_barrier() {
        let w = wait(
            WaitKind::JobCompletion {
                job_id: "job-9".into(),
            },
            Some(at(2026, 7, 14, 18, 0)),
        );
        assert!(!w.timed_out(at(2026, 7, 14, 17, 59)));
        assert!(w.timed_out(at(2026, 7, 14, 18, 0)));
        assert!(w.timed_out(at(2026, 7, 14, 18, 1)));
        // No timeout means it never times out.
        let forever = wait(WaitKind::HumanInput, None);
        assert!(!forever.timed_out(at(2999, 1, 1, 0, 0)));
    }

    #[test]
    fn satisfy_records_the_note_and_resolution_time() {
        let mut w = wait(
            WaitKind::ProcessExit {
                handle: "pty-7".into(),
            },
            None,
        );
        let t = at(2026, 7, 14, 10, 0);
        w.satisfy(Some("exit status 0".into()), t).unwrap();
        assert_eq!(w.state, WaitState::Satisfied);
        assert!(!w.is_active());
        assert_eq!(w.note.as_deref(), Some("exit status 0"));
        assert_eq!(w.resolved_at, Some(t));
        assert_eq!(w.updated_at, t);
    }

    #[test]
    fn time_out_labels_the_barrier() {
        let mut w = wait(
            WaitKind::JobCompletion {
                job_id: "job-9".into(),
            },
            Some(at(2026, 7, 14, 18, 0)),
        );
        w.time_out(at(2026, 7, 14, 18, 0)).unwrap();
        assert_eq!(w.state, WaitState::TimedOut);
        assert_eq!(w.note.as_deref(), Some("wait for job_completion timed out"));
        assert_eq!(w.resolved_at, Some(at(2026, 7, 14, 18, 0)));
    }

    #[test]
    fn a_resolved_wait_rejects_a_second_resolution() {
        let mut w = wait(WaitKind::HumanInput, None);
        w.satisfy(None, at(2026, 7, 14, 10, 0)).unwrap();
        // A late barrier fire cannot rewrite the outcome.
        assert!(w
            .satisfy(Some("late".into()), at(2026, 7, 14, 10, 1))
            .is_err());
        assert!(w.time_out(at(2026, 7, 14, 10, 2)).is_err());
        assert_eq!(w.state, WaitState::Satisfied);
        assert!(w.note.is_none());
    }

    #[test]
    fn cancel_is_idempotent_and_only_fires_once() {
        let mut w = wait(WaitKind::Event { key: "ci".into() }, None);
        let t = at(2026, 7, 14, 10, 0);
        assert!(w.cancel("goal paused", t));
        assert_eq!(w.state, WaitState::Cancelled);
        assert_eq!(w.note.as_deref(), Some("goal paused"));
        // Second cancel is a no-op and does not overwrite the reason.
        assert!(!w.cancel("something else", at(2026, 7, 14, 10, 5)));
        assert_eq!(w.note.as_deref(), Some("goal paused"));
    }

    #[test]
    fn cancel_cannot_override_a_real_resolution() {
        let mut w = wait(WaitKind::HumanInput, None);
        w.satisfy(None, at(2026, 7, 14, 10, 0)).unwrap();
        assert!(!w.cancel("too late", at(2026, 7, 14, 10, 1)));
        assert_eq!(w.state, WaitState::Satisfied);
    }

    #[test]
    fn describe_names_the_barrier_and_timeout() {
        let w = wait(
            WaitKind::OutputPattern {
                handle: "pty-3".into(),
                pattern: "BUILD SUCCESSFUL".into(),
            },
            Some(at(2026, 7, 14, 18, 0)),
        );
        let d = w.describe();
        assert!(d.contains("pty-3"), "{d}");
        assert!(d.contains("BUILD SUCCESSFUL"), "{d}");
        assert!(d.contains("timeout 2026-07-14 18:00"), "{d}");

        let human = wait(WaitKind::HumanInput, None);
        assert_eq!(human.describe(), "waiting for a reply from you");
    }

    #[test]
    fn kind_round_trips_through_json_with_a_stable_tag() {
        let w = wait(
            WaitKind::Until {
                at: at(2026, 7, 14, 12, 0),
            },
            None,
        );
        let json = serde_json::to_string(&w).unwrap();
        assert!(json.contains("\"barrier\":\"until\""), "{json}");
        let back: Wait = serde_json::from_str(&json).unwrap();
        assert_eq!(back, w);
    }

    // ---- WaitStore ----------------------------------------------------

    fn store() -> (tempfile::TempDir, WaitStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = WaitStore::new(dir.path().join("waits.json"));
        (dir, store)
    }

    #[test]
    fn a_missing_file_lists_and_arms_cleanly() {
        let (_dir, store) = store();
        assert!(store.list().unwrap().is_empty());
        assert!(store.armed().unwrap().is_empty());
        let w = store
            .arm(
                "goal-1",
                owner(),
                WaitKind::HumanInput,
                None,
                at(2026, 7, 14, 9, 0),
            )
            .unwrap();
        assert_eq!(w.state, WaitState::Armed);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn arm_assigns_unique_ids_across_goals() {
        let (_dir, store) = store();
        let a = store
            .arm(
                "goal-1",
                owner(),
                WaitKind::HumanInput,
                None,
                at(2026, 7, 14, 9, 0),
            )
            .unwrap();
        let b = store
            .arm(
                "goal-2",
                owner(),
                WaitKind::Event { key: "ci".into() },
                None,
                at(2026, 7, 14, 9, 0),
            )
            .unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(store.armed().unwrap().len(), 2);
    }

    #[test]
    fn arm_supersedes_a_goals_earlier_barrier() {
        let (_dir, store) = store();
        let first = store
            .arm(
                "goal-1",
                owner(),
                WaitKind::Until {
                    at: at(2026, 7, 14, 12, 0),
                },
                None,
                at(2026, 7, 14, 9, 0),
            )
            .unwrap();
        let second = store
            .arm(
                "goal-1",
                owner(),
                WaitKind::JobCompletion {
                    job_id: "job-9".into(),
                },
                None,
                at(2026, 7, 14, 9, 30),
            )
            .unwrap();
        let armed = store.armed_for_goal("goal-1").unwrap();
        assert_eq!(armed.len(), 1, "a goal parks on exactly one barrier");
        assert_eq!(armed[0].id, second.id);
        assert!(store.get(&first.id).unwrap().is_none());
    }

    #[test]
    fn update_is_revisioned_and_rejects_stale_snapshots() {
        let (_dir, store) = store();
        let w = store
            .arm(
                "goal-1",
                owner(),
                WaitKind::Event { key: "ci".into() },
                None,
                at(2026, 7, 14, 9, 0),
            )
            .unwrap();
        // A first update from the loaded snapshot succeeds and bumps revision.
        let mut edit = w.clone();
        edit.timeout = Some(at(2026, 7, 14, 18, 0));
        let persisted = store.update(&edit).unwrap();
        assert_eq!(persisted.revision, w.revision + 1);
        // The now-stale original snapshot is rejected.
        assert!(store.update(&w).is_err());
        // Updating an unknown id never silently creates.
        let ghost = wait(WaitKind::HumanInput, None);
        assert!(store.update(&ghost).is_err());
    }

    #[test]
    fn remove_reports_whether_it_deleted() {
        let (_dir, store) = store();
        let w = store
            .arm(
                "goal-1",
                owner(),
                WaitKind::HumanInput,
                None,
                at(2026, 7, 14, 9, 0),
            )
            .unwrap();
        assert!(store.remove(&w.id).unwrap());
        assert!(!store.remove(&w.id).unwrap());
        assert!(store.armed().unwrap().is_empty());
    }

    #[test]
    fn cancel_for_goal_removes_only_that_goals_waits() {
        let (_dir, store) = store();
        store
            .arm(
                "goal-1",
                owner(),
                WaitKind::HumanInput,
                None,
                at(2026, 7, 14, 9, 0),
            )
            .unwrap();
        let other = store
            .arm(
                "goal-2",
                owner(),
                WaitKind::Event { key: "ci".into() },
                None,
                at(2026, 7, 14, 9, 0),
            )
            .unwrap();
        let cancelled = store.cancel_for_goal("goal-1").unwrap();
        assert_eq!(cancelled.len(), 1);
        let remaining = store.armed().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, other.id);
        // Cancelling a goal with no waits is a clean empty result.
        assert!(store.cancel_for_goal("goal-1").unwrap().is_empty());
    }

    #[test]
    fn take_human_input_satisfies_only_human_waits_on_the_lane() {
        let (_dir, store) = store();
        let human = store
            .arm(
                "goal-1",
                owner(),
                WaitKind::HumanInput,
                None,
                at(2026, 7, 14, 9, 0),
            )
            .unwrap();
        // A non-human barrier on the same owner is untouched.
        store
            .arm(
                "goal-2",
                owner(),
                WaitKind::Event { key: "ci".into() },
                None,
                at(2026, 7, 14, 9, 0),
            )
            .unwrap();
        // A human barrier on a different owner is untouched.
        let other_owner = OwnerKey::from_parts(&["telegram", "private", "99"]);
        store
            .arm(
                "goal-3",
                other_owner,
                WaitKind::HumanInput,
                None,
                at(2026, 7, 14, 9, 0),
            )
            .unwrap();

        let taken = store
            .take_human_input_for_owner(&owner(), at(2026, 7, 14, 10, 0))
            .unwrap();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].id, human.id);
        assert_eq!(taken[0].state, WaitState::Satisfied);
        assert_eq!(taken[0].goal_id, "goal-1");
        // Only the one human wait on the owner was consumed.
        assert_eq!(store.armed().unwrap().len(), 2);
        // A second call finds nothing left to take.
        assert!(store
            .take_human_input_for_owner(&owner(), at(2026, 7, 14, 10, 1))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn arm_persists_across_store_instances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("waits.json");
        let w = WaitStore::new(&path)
            .arm(
                "goal-1",
                owner(),
                WaitKind::ProcessExit {
                    handle: "pty-7".into(),
                },
                Some(at(2026, 7, 14, 18, 0)),
                at(2026, 7, 14, 9, 0),
            )
            .unwrap();
        // A fresh instance over the same path sees the persisted wait.
        let reloaded = WaitStore::new(&path).get(&w.id).unwrap().unwrap();
        assert_eq!(reloaded, w);
    }
}
