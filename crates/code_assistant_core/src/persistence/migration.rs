//! One-time move of flat session files into session folders.
//!
//! Sessions used to be `sessions/<id>.json` files with IDs like
//! `chat_6ac611f7_35e_0`, next to `<id>.ui_state.json` and with drafts in a
//! separate directory. Each one becomes a session folder with a new ID (see
//! `persistence::layout`), so everything that stores IDs is rewritten:
//! `metadata.json`, `lifecycle.json`, and the session owner keys in
//! `goals.json` and `waits.json`. `sessions/legacy-ids.json` records the
//! mapping from old to new IDs, and the old files move to `sessions/legacy/`.
//!
//! The run goes in phases so that it can be restarted at any point:
//!
//! 1. Read each old session's project and creation date.
//! 2. Allocate the new IDs in creation order and record the mapping.
//! 3. Write the new sessions (in parallel; most of the time is spent
//!    waiting for the disk) and move their UI state, draft and log along.
//! 4. Rewrite the IDs in the index, the lifecycles and the goal stores.
//! 5. Move the old session files away, and with them the leftovers of
//!    sessions deleted long ago (locks, UI states, drafts).
//!
//! A rerun takes the recorded mapping, skips sessions that already have a
//! journal, and repeats the idempotent rest. Until the mapping is recorded,
//! each reserved folder holds a `migration-reservation` file naming its old
//! session, so a run interrupted in phase 2 leaves no gaps: the next one
//! takes over those folders. (An empty folder alone could also be an ACP
//! reservation of a running instance.)

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;
use tracing::{info, warn};

use super::{ChatMetadata, ChatSession, FileSessionPersistence};
use crate::utils::file_utils::{self, atomic_write_json, lock_exclusive};

const ALIASES_FILE: &str = "legacy-ids.json";
const LEGACY_DIR: &str = "legacy";
const LOCK_FILE: &str = "migration.lock";
/// Names the old session a folder was reserved for, until the mapping is
/// recorded.
const RESERVATION_FILE: &str = "migration-reservation";
/// Old sessions written at the same time. Each is parsed whole, and the
/// largest are several hundred megabytes.
const PARALLEL_WRITERS: usize = 4;

/// How far a migration run is, reported as it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationProgress {
    pub phase: MigrationPhase,
    /// Sessions done in this phase, out of `total`.
    pub done: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationPhase {
    /// Reading the old sessions to allocate their new IDs.
    Reading,
    /// Writing the new sessions.
    Writing,
    /// Rewriting references and moving the old files away.
    Finishing,
}

impl MigrationPhase {
    /// What the phase does, for progress displays.
    pub fn description(self) -> &'static str {
        match self {
            MigrationPhase::Reading => "Reading sessions",
            MigrationPhase::Writing => "Writing sessions",
            MigrationPhase::Finishing => "Finishing up",
        }
    }
}

impl MigrationProgress {
    /// How far the whole run is, from 0 to 1. Writing takes most of the
    /// time, so it gets most of the range.
    pub fn fraction(&self) -> f32 {
        let (start, width) = match self.phase {
            MigrationPhase::Reading => (0.0, 0.25),
            MigrationPhase::Writing => (0.25, 0.7),
            MigrationPhase::Finishing => (0.95, 0.05),
        };
        let within = if self.total == 0 {
            0.0
        } else {
            self.done as f32 / self.total as f32
        };
        start + width * within
    }
}

/// What a migration run did.
#[derive(Debug, Default)]
pub struct MigrationReport {
    /// Old and new ID of every session moved in this run.
    pub migrated: Vec<(String, String)>,
    /// Sessions left in place, with the reason.
    pub skipped: Vec<(String, String)>,
}

/// An old session file awaiting migration.
struct Legacy {
    old_id: String,
    path: PathBuf,
}

/// The part of an old session file that decides its new ID. Everything
/// else is skipped while parsing.
#[derive(Deserialize)]
struct LegacyHeader {
    created_at: SystemTime,
    #[serde(default)]
    config: LegacyConfig,
    /// Where very old files kept the project root.
    #[serde(default)]
    init_path: Option<PathBuf>,
}

#[derive(Deserialize, Default)]
struct LegacyConfig {
    #[serde(default)]
    init_path: Option<PathBuf>,
}

impl FileSessionPersistence {
    /// Whether there are flat session files to migrate. Cheap: one
    /// directory listing.
    pub fn has_legacy_sessions(&self) -> bool {
        legacy_session_files(self.layout.sessions_dir()).is_ok_and(|files| !files.is_empty())
    }

    /// Move flat session files into session folders, reporting `progress`
    /// as it goes. Cheap when there is nothing to move; otherwise holds a
    /// lock so a second process waits instead of migrating in parallel.
    /// `legacy_drafts_dir` is where drafts used to be kept.
    pub fn migrate_legacy_sessions(
        &mut self,
        legacy_drafts_dir: &Path,
        progress: &(dyn Fn(MigrationProgress) + Sync),
    ) -> Result<MigrationReport> {
        let sessions_dir = self.layout.sessions_dir().to_path_buf();
        if !self.has_legacy_sessions() {
            return Ok(MigrationReport::default());
        }
        let _lock = lock_exclusive(&sessions_dir.join(LOCK_FILE))?;
        // Another process may have migrated while we waited.
        let files = legacy_session_files(&sessions_dir)?;
        info!("Migrating {} sessions to session folders", files.len());
        let mut report = MigrationReport::default();

        // Phases 1 and 2: new IDs, in creation order.
        let aliases_path = sessions_dir.join(ALIASES_FILE);
        let mut aliases: BTreeMap<String, String> = read_json(&aliases_path)?.unwrap_or_default();
        let mut reserved = self.read_reservations()?;
        let total = files.len();
        let mut pending = Vec::new();
        for (index, legacy) in files.into_iter().enumerate() {
            progress(MigrationProgress {
                phase: MigrationPhase::Reading,
                done: index,
                total,
            });
            let agent_lock = sessions_dir.join(format!("{}.agent.lock", legacy.old_id));
            if file_utils::is_agent_locked(&agent_lock) {
                report
                    .skipped
                    .push((legacy.old_id, "an agent is running in it".into()));
                continue;
            }
            if !aliases.contains_key(&legacy.old_id) {
                let allocated = match reserved.remove(&legacy.old_id) {
                    Some(new_id) => Ok(new_id),
                    None => self.allocate_for(&legacy),
                };
                match allocated {
                    Ok(new_id) => {
                        aliases.insert(legacy.old_id.clone(), new_id);
                    }
                    Err(e) => {
                        report.skipped.push((legacy.old_id, format!("{e:#}")));
                        continue;
                    }
                }
            }
            pending.push(legacy);
        }
        atomic_write_json(&aliases_path, &aliases)?;
        // The mapping is recorded; reservations of sessions that turned out
        // unreadable are given up.
        for session_id in self.layout.ids_of_folders_with(RESERVATION_FILE)? {
            let session_dir = self.layout.session_dir(&session_id)?;
            std::fs::remove_file(session_dir.join(RESERVATION_FILE))?;
            if !aliases.values().any(|new_id| *new_id == session_id) {
                let _ = std::fs::remove_dir(&session_dir);
            }
        }

        // Phase 3: the new sessions.
        let written = self.write_migrated(&pending, &aliases, legacy_drafts_dir, progress);
        let mut metadata = Vec::new();
        let mut retire_list = Vec::new();
        for (legacy, result) in pending.iter().zip(written) {
            let new_id = aliases[&legacy.old_id].clone();
            match result {
                Ok(entry) => {
                    metadata.push(entry);
                    retire_list.push(legacy);
                    report.migrated.push((legacy.old_id.clone(), new_id));
                }
                Err(e) => {
                    warn!("Not migrating session {}: {e:#}", legacy.old_id);
                    report
                        .skipped
                        .push((legacy.old_id.clone(), format!("{e:#}")));
                }
            }
        }

        // Phase 4: references to the old IDs.
        progress(MigrationProgress {
            phase: MigrationPhase::Finishing,
            done: 0,
            total: report.migrated.len(),
        });
        if !report.migrated.is_empty() {
            let renamed: BTreeMap<&str, &str> = report
                .migrated
                .iter()
                .map(|(old, new)| (old.as_str(), new.as_str()))
                .collect();
            self.rename_in_index(&renamed, metadata)?;
            let root = sessions_dir.parent().unwrap_or(&sessions_dir);
            for store in ["goals.json", "waits.json"] {
                rename_session_owners(&root.join(store), &renamed)?;
            }
        }

        // Phase 5: the old files.
        for legacy in retire_list {
            retire(&sessions_dir, legacy)?;
        }
        sweep_orphans(&sessions_dir, legacy_drafts_dir)?;
        info!(
            "Migrated {} sessions, skipped {}",
            report.migrated.len(),
            report.skipped.len()
        );
        Ok(report)
    }

    /// The folders an interrupted run reserved, by old session ID.
    fn read_reservations(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut reserved = std::collections::HashMap::new();
        for session_id in self.layout.ids_of_folders_with(RESERVATION_FILE)? {
            let marker = self.layout.session_dir(&session_id)?.join(RESERVATION_FILE);
            let old_id = std::fs::read_to_string(marker)?;
            reserved.insert(old_id.trim().to_string(), session_id);
        }
        Ok(reserved)
    }

    /// Reserve the new ID of an old session from its project and creation
    /// date, marking the folder as reserved for it.
    fn allocate_for(&self, legacy: &Legacy) -> Result<String> {
        let json = std::fs::read_to_string(&legacy.path)?;
        let header: LegacyHeader = serde_json::from_str(&json)
            .with_context(|| format!("failed to parse {}", legacy.path.display()))?;
        let project_root = header.config.init_path.or(header.init_path);
        let created = chrono::DateTime::<chrono::Local>::from(header.created_at).date_naive();
        let new_id = self
            .layout
            .allocate_session_id(project_root.as_deref(), created)?;
        // Not synced: a marker lost to a power failure only leaves a gap.
        std::fs::write(
            self.layout.session_dir(&new_id)?.join(RESERVATION_FILE),
            &legacy.old_id,
        )?;
        Ok(new_id)
    }

    /// Write every pending session under its new ID, a few at a time, and
    /// return each one's metadata (or why it failed), in order.
    fn write_migrated(
        &self,
        pending: &[Legacy],
        aliases: &BTreeMap<String, String>,
        legacy_drafts_dir: &Path,
        progress: &(dyn Fn(MigrationProgress) + Sync),
    ) -> Vec<Result<ChatMetadata>> {
        let next = AtomicUsize::new(0);
        let finished = AtomicUsize::new(0);
        let total = pending.len();
        progress(MigrationProgress {
            phase: MigrationPhase::Writing,
            done: 0,
            total,
        });
        let results: Mutex<Vec<Option<Result<ChatMetadata>>>> =
            Mutex::new((0..pending.len()).map(|_| None).collect());
        std::thread::scope(|scope| {
            for _ in 0..PARALLEL_WRITERS.min(pending.len()) {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(legacy) = pending.get(index) else {
                            break;
                        };
                        let new_id = &aliases[&legacy.old_id];
                        let result =
                            self.write_migrated_session(legacy, new_id)
                                .and_then(|metadata| {
                                    self.move_side_files(
                                        &legacy.old_id,
                                        new_id,
                                        legacy_drafts_dir,
                                    )?;
                                    Ok(metadata)
                                });
                        results.lock().unwrap()[index] = Some(result);
                        progress(MigrationProgress {
                            phase: MigrationPhase::Writing,
                            done: finished.fetch_add(1, Ordering::Relaxed) + 1,
                            total,
                        });
                    }
                });
            }
        });
        results
            .into_inner()
            .unwrap()
            .into_iter()
            .map(|result| result.expect("every pending session is written"))
            .collect()
    }

    /// Write one old session under its new ID, unless an interrupted run
    /// already did.
    fn write_migrated_session(&self, legacy: &Legacy, new_id: &str) -> Result<ChatMetadata> {
        let json = std::fs::read_to_string(&legacy.path)?;
        let mut session: ChatSession = serde_json::from_str(&json)
            .with_context(|| format!("failed to parse {}", legacy.path.display()))?;
        drop(json);
        session.id = new_id.to_string();
        if self.layout.journal(new_id)?.exists() {
            session.ensure_config()?;
            return Ok(session.metadata());
        }
        self.write_new_session(session)
    }

    /// Move the UI state, the draft and the diagnostics log along.
    fn move_side_files(&self, old_id: &str, new_id: &str, legacy_drafts_dir: &Path) -> Result<()> {
        let sessions_dir = self.layout.sessions_dir();
        move_if_present(
            &sessions_dir.join(format!("{old_id}.ui_state.json")),
            &self.layout.ui_state(new_id)?,
        )?;
        let session_dir = self.layout.session_dir(new_id)?;
        move_if_present(
            &sessions_dir.join(format!("{old_id}.diag.log")),
            &session_dir.join("diag.log"),
        )?;

        let draft_path = legacy_drafts_dir.join(format!("{old_id}.json"));
        if let Some(mut draft) = read_json::<serde_json::Value>(&draft_path)? {
            draft["session_id"] = serde_json::Value::String(new_id.to_string());
            atomic_write_json(&self.layout.draft(new_id)?, &draft)?;
            std::fs::remove_file(&draft_path)?;
        }
        Ok(())
    }

    /// Put the migrated sessions into `metadata.json` in place of their old
    /// entries, and move their lifecycles to the new IDs.
    fn rename_in_index(
        &self,
        renamed: &BTreeMap<&str, &str>,
        metadata: Vec<ChatMetadata>,
    ) -> Result<()> {
        {
            let _lock = lock_exclusive(&self.metadata_lock_path()?)?;
            let path = self.metadata_file_path()?;
            let mut list: Vec<ChatMetadata> = read_json(&path)?.unwrap_or_default();
            let new_ids: std::collections::HashSet<&str> =
                metadata.iter().map(|entry| entry.id.as_str()).collect();
            list.retain(|entry| {
                !renamed.contains_key(entry.id.as_str()) && !new_ids.contains(entry.id.as_str())
            });
            list.extend(metadata);
            atomic_write_json(&path, &list)?;
        }
        let _lock = lock_exclusive(&self.lifecycle_lock_path()?)?;
        let mut lifecycles = self.read_lifecycles_unlocked()?;
        let mut changed = false;
        for (old, new) in renamed {
            if let Some(lifecycle) = lifecycles.remove(*old) {
                lifecycles.insert(new.to_string(), lifecycle);
                changed = true;
            }
        }
        if changed {
            atomic_write_json(&self.lifecycle_file_path()?, &lifecycles)?;
        }
        Ok(())
    }
}

/// Flat session files in the sessions directory, oldest first (by the
/// creation time encoded in their ID).
fn legacy_session_files(sessions_dir: &Path) -> Result<Vec<Legacy>> {
    let entries = match std::fs::read_dir(sessions_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut files: Vec<Legacy> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter_map(|path| {
            let old_id = path
                .file_name()?
                .to_str()?
                .strip_suffix(".json")?
                .to_string();
            let is_session = !old_id.ends_with(".ui_state")
                && !matches!(old_id.as_str(), "metadata" | "lifecycle" | "legacy-ids");
            is_session.then_some(Legacy { old_id, path })
        })
        .collect();
    files.sort_by_key(|legacy| (legacy_timestamp(&legacy.old_id), legacy.old_id.clone()));
    Ok(files)
}

/// The creation time in an ID like `chat_6ac611f7_35e_0` (hex seconds).
fn legacy_timestamp(id: &str) -> Option<u64> {
    let hex = id.strip_prefix("chat_")?.split('_').next()?;
    u64::from_str_radix(hex, 16).ok()
}

/// Move a migrated session's old file to `legacy/` and remove its locks.
fn retire(sessions_dir: &Path, legacy: &Legacy) -> Result<()> {
    let legacy_dir = sessions_dir.join(LEGACY_DIR);
    std::fs::create_dir_all(&legacy_dir)?;
    std::fs::rename(
        &legacy.path,
        legacy_dir.join(format!("{}.json", legacy.old_id)),
    )?;
    for lock in ["entry.lock", "agent.lock"] {
        let _ = std::fs::remove_file(sessions_dir.join(format!("{}.{lock}", legacy.old_id)));
    }
    Ok(())
}

/// Remove what is left of sessions that have no file any more: stale locks
/// go, UI states, logs and drafts move to `legacy/`. Files of sessions that
/// were skipped (their session file is still there) stay.
fn sweep_orphans(sessions_dir: &Path, legacy_drafts_dir: &Path) -> Result<()> {
    let legacy_dir = sessions_dir.join(LEGACY_DIR);
    let orphaned = |old_id: &str| !sessions_dir.join(format!("{old_id}.json")).exists();

    for entry in std::fs::read_dir(sessions_dir)?.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(old_id) = name
            .strip_suffix(".entry.lock")
            .or_else(|| name.strip_suffix(".agent.lock"))
        {
            if orphaned(old_id) && !file_utils::is_agent_locked(&path) {
                std::fs::remove_file(&path)?;
            }
        } else if let Some(old_id) = name
            .strip_suffix(".ui_state.json")
            .or_else(|| name.strip_suffix(".diag.log"))
            && orphaned(old_id)
        {
            std::fs::create_dir_all(&legacy_dir)?;
            std::fs::rename(&path, legacy_dir.join(name))?;
        }
    }

    let Ok(drafts) = std::fs::read_dir(legacy_drafts_dir) else {
        return Ok(());
    };
    for entry in drafts.flatten() {
        let path = entry.path();
        let Some(old_id) = path.file_stem().and_then(|n| n.to_str()) else {
            continue;
        };
        if orphaned(old_id) {
            let target = legacy_dir.join("drafts");
            std::fs::create_dir_all(&target)?;
            std::fs::rename(&path, target.join(entry.file_name()))?;
        }
    }
    // Only succeeds once the directory is empty.
    let _ = std::fs::remove_dir(legacy_drafts_dir);
    Ok(())
}

/// Replace `session:<old>` owner keys in a goal or wait store, if it exists.
fn rename_session_owners(path: &Path, renamed: &BTreeMap<&str, &str>) -> Result<()> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let _lock = lock_exclusive(&path.with_extension("json.lock"))?;
    let mut updated = content.clone();
    for (old, new) in renamed {
        updated = updated.replace(&format!("\"session:{old}\""), &format!("\"session:{new}\""));
    }
    if updated != content {
        file_utils::atomic_write(path, updated.as_bytes())?;
    }
    Ok(())
}

fn move_if_present(from: &Path, to: &Path) -> Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            Ok(Some(serde_json::from_str(&content).with_context(|| {
                format!("failed to parse {}", path.display())
            })?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionConfig;
    use crate::session::lifecycle::SessionLifecycle;
    use llm::Message;
    use std::time::{Duration, SystemTime};
    use tempfile::tempdir;

    /// A data directory in the old layout, with one session of the given
    /// old ID started in `/w/proj` at the given second.
    struct Legacy {
        root: tempfile::TempDir,
    }

    impl Legacy {
        fn new() -> Self {
            let legacy = Self {
                root: tempdir().unwrap(),
            };
            std::fs::create_dir_all(legacy.sessions()).unwrap();
            legacy
        }

        fn sessions(&self) -> PathBuf {
            self.root.path().join("sessions")
        }

        fn drafts(&self) -> PathBuf {
            self.root.path().join("drafts")
        }

        fn add_session(&self, old_id: &str, created_secs: u64) -> ChatSession {
            let config = SessionConfig {
                init_path: Some(PathBuf::from("/w/proj")),
                ..SessionConfig::default()
            };
            let mut session = ChatSession::new_empty(old_id.into(), "old".into(), config, None);
            session.created_at = SystemTime::UNIX_EPOCH + Duration::from_secs(created_secs);
            session.add_message(Message::new_user("hello"));
            session.tool_executions = vec![agent_core::SerializedToolExecution {
                tool_request: agent_core::ToolRequest {
                    id: "t1".into(),
                    name: "read_files".into(),
                    input: serde_json::json!({}),
                    start_offset: None,
                    end_offset: None,
                },
                result_json: serde_json::json!({ "content": "x".repeat(20_000) }),
                tool_name: "read_files".into(),
            }];
            std::fs::write(
                self.sessions().join(format!("{old_id}.json")),
                serde_json::to_string(&session).unwrap(),
            )
            .unwrap();
            session
        }

        fn write(&self, relative: &str, content: &str) {
            let path = self.root.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }

        fn read(&self, relative: &str) -> String {
            std::fs::read_to_string(self.root.path().join(relative)).unwrap()
        }

        fn persistence(&self) -> FileSessionPersistence {
            FileSessionPersistence::new_with_root_dir(self.root.path().to_path_buf())
        }

        fn migrate(&self) -> MigrationReport {
            self.persistence()
                .migrate_legacy_sessions(&self.drafts(), &|_| {})
                .unwrap()
        }
    }

    // 2026-10-07 12:00 UTC
    const OCT_7: u64 = 1_791_374_400;

    #[test]
    fn sessions_move_into_folders_with_everything_that_refers_to_them() {
        let legacy = Legacy::new();
        let old = legacy.add_session("chat_a", OCT_7);
        legacy.write(
            "sessions/chat_a.ui_state.json",
            r#"{"plan_collapsed":true}"#,
        );
        legacy.write("sessions/chat_a.diag.log", "log");
        legacy.write("sessions/chat_a.entry.lock", "");
        legacy.write(
            "drafts/chat_a.json",
            r#"{"session_id":"chat_a","message":"unsent","attachments":[]}"#,
        );
        legacy.write(
            "sessions/metadata.json",
            &serde_json::to_string(&[old.metadata()]).unwrap(),
        );
        let lifecycles = BTreeMap::from([(
            "chat_a".to_string(),
            SessionLifecycle {
                last_visited_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(OCT_7)),
                ..Default::default()
            },
        )]);
        legacy.write(
            "sessions/lifecycle.json",
            &serde_json::to_string(&lifecycles).unwrap(),
        );
        legacy.write(
            "goals.json",
            r#"{"owners":["session:chat_a","session:chat_ab"]}"#,
        );

        let report = legacy.migrate();

        let new_id = "-w-proj/2026-10-07-001";
        assert_eq!(
            report.migrated,
            [("chat_a".to_string(), new_id.to_string())]
        );
        let persistence = legacy.persistence();
        let session = persistence.load_chat_session(new_id).unwrap().unwrap();
        assert_eq!(session.id, new_id);
        assert_eq!(session.message_count(), 1);
        assert_eq!(
            session.tool_executions[0].result_json,
            old.tool_executions[0].result_json
        );
        assert_eq!(session.created_at, old.created_at);

        let dir = format!("sessions/{new_id}");
        assert_eq!(
            legacy.read(&format!("{dir}/ui_state.json")),
            r#"{"plan_collapsed":true}"#
        );
        assert_eq!(legacy.read(&format!("{dir}/diag.log")), "log");
        assert!(legacy.read(&format!("{dir}/draft.json")).contains(new_id));
        assert!(!legacy.drafts().join("chat_a.json").exists());

        let ids: Vec<_> = persistence
            .list_chat_sessions()
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, [new_id]);
        assert!(
            persistence.load_lifecycles().unwrap()[new_id]
                .last_visited_at
                .is_some()
        );
        assert_eq!(
            legacy.read("goals.json"),
            format!(r#"{{"owners":["session:{new_id}","session:chat_ab"]}}"#)
        );

        assert!(legacy.sessions().join("legacy/chat_a.json").exists());
        assert!(!legacy.sessions().join("chat_a.json").exists());
        assert!(!legacy.sessions().join("chat_a.entry.lock").exists());
        assert!(legacy.read("sessions/legacy-ids.json").contains(new_id));

        // Nothing left to do.
        assert!(legacy.migrate().migrated.is_empty());
    }

    #[test]
    fn numbers_follow_creation_order_per_day() {
        let legacy = Legacy::new();
        // Names sort differently than the timestamps they encode.
        legacy.add_session(&format!("chat_{:x}_1_0", OCT_7 + 60), OCT_7 + 60);
        legacy.add_session(&format!("chat_{:x}_2_0", OCT_7), OCT_7);
        legacy.add_session(&format!("chat_{:x}_3_0", OCT_7 + 86_400), OCT_7 + 86_400);

        let new_ids: Vec<_> = legacy
            .migrate()
            .migrated
            .into_iter()
            .map(|(_, new)| new)
            .collect();

        assert_eq!(
            new_ids,
            [
                "-w-proj/2026-10-07-001",
                "-w-proj/2026-10-07-002",
                "-w-proj/2026-10-08-001"
            ]
        );
    }

    #[test]
    fn a_rerun_finishes_an_interrupted_move_without_a_duplicate() {
        let legacy = Legacy::new();
        legacy.add_session("chat_a", OCT_7);
        legacy.migrate();
        // Simulate a crash after the session was created, before its old
        // file was moved away.
        std::fs::rename(
            legacy.sessions().join("legacy/chat_a.json"),
            legacy.sessions().join("chat_a.json"),
        )
        .unwrap();

        let report = legacy.migrate();

        assert_eq!(report.migrated.len(), 1);
        assert_eq!(legacy.persistence().list_chat_sessions().unwrap().len(), 1);
        assert!(!legacy.sessions().join("-w-proj/2026-10-07-002").exists());
    }

    #[test]
    fn a_rerun_uses_the_id_allocated_before_an_interruption() {
        let legacy = Legacy::new();
        legacy.add_session("chat_a", OCT_7);
        // Interrupted after the ID was recorded and its folder reserved,
        // before the session was written.
        legacy.write(
            "sessions/legacy-ids.json",
            r#"{"chat_a":"-w-proj/2026-10-07-001"}"#,
        );
        std::fs::create_dir_all(legacy.sessions().join("-w-proj/2026-10-07-001")).unwrap();

        let report = legacy.migrate();

        assert_eq!(
            report.migrated,
            [("chat_a".to_string(), "-w-proj/2026-10-07-001".to_string())]
        );
        assert!(
            legacy
                .persistence()
                .load_chat_session("-w-proj/2026-10-07-001")
                .unwrap()
                .is_some()
        );
        assert!(!legacy.sessions().join("-w-proj/2026-10-07-002").exists());
    }

    #[test]
    fn leftovers_of_long_deleted_sessions_are_cleared() {
        let legacy = Legacy::new();
        legacy.add_session("chat_a", OCT_7);
        legacy.write("sessions/chat_gone.entry.lock", "");
        legacy.write("sessions/chat_gone.ui_state.json", "{}");
        legacy.write("drafts/chat_gone.json", "{}");
        legacy.write("sessions/chat_broken.json", "{ not json");
        legacy.write("sessions/chat_broken.entry.lock", "");
        legacy.write("sessions/chat_broken.ui_state.json", "{}");
        legacy.write("drafts/chat_broken.json", "{}");

        legacy.migrate();

        let sessions = legacy.sessions();
        assert!(!sessions.join("chat_gone.entry.lock").exists());
        assert!(sessions.join("legacy/chat_gone.ui_state.json").exists());
        assert!(sessions.join("legacy/drafts/chat_gone.json").exists());
        // The skipped session keeps everything.
        for kept in ["chat_broken.entry.lock", "chat_broken.ui_state.json"] {
            assert!(sessions.join(kept).exists(), "{kept}");
        }
        assert!(legacy.drafts().join("chat_broken.json").exists());
    }

    #[test]
    fn a_rerun_takes_over_the_folders_an_interrupted_run_reserved() {
        let legacy = Legacy::new();
        legacy.add_session("chat_a", OCT_7);
        legacy.add_session("chat_b", OCT_7 + 60);
        // Interrupted while allocating, before the mapping was recorded:
        // chat_a got 001, a folder reserved for a session that is gone
        // got 002.
        legacy.write(
            "sessions/-w-proj/2026-10-07-001/migration-reservation",
            "chat_a",
        );
        legacy.write(
            "sessions/-w-proj/2026-10-07-002/migration-reservation",
            "chat_gone",
        );

        let report = legacy.migrate();

        assert_eq!(
            report.migrated,
            [
                ("chat_a".to_string(), "-w-proj/2026-10-07-001".to_string()),
                ("chat_b".to_string(), "-w-proj/2026-10-07-003".to_string()),
            ]
        );
        let sessions = legacy.sessions();
        assert!(
            !sessions
                .join("-w-proj/2026-10-07-001/migration-reservation")
                .exists()
        );
        assert!(!sessions.join("-w-proj/2026-10-07-002").exists());
        assert!(
            !sessions
                .join("-w-proj/2026-10-07-003/migration-reservation")
                .exists()
        );
    }

    #[test]
    fn progress_is_reported_per_session_and_phase() {
        let legacy = Legacy::new();
        legacy.add_session("chat_a", OCT_7);
        legacy.add_session("chat_b", OCT_7 + 60);
        let reports = Mutex::new(Vec::new());

        legacy
            .persistence()
            .migrate_legacy_sessions(&legacy.drafts(), &|progress| {
                reports.lock().unwrap().push(progress)
            })
            .unwrap();

        let reports = reports.into_inner().unwrap();
        let in_phase = |phase| -> Vec<(usize, usize)> {
            reports
                .iter()
                .filter(|p| p.phase == phase)
                .map(|p| (p.done, p.total))
                .collect()
        };
        assert_eq!(in_phase(MigrationPhase::Reading), [(0, 2), (1, 2)]);
        assert_eq!(in_phase(MigrationPhase::Writing), [(0, 2), (1, 2), (2, 2)]);
        assert_eq!(in_phase(MigrationPhase::Finishing), [(0, 2)]);
    }

    #[test]
    fn the_fraction_runs_through_the_phases() {
        let at = |phase, done, total| MigrationProgress { phase, done, total }.fraction();
        assert_eq!(at(MigrationPhase::Reading, 0, 10), 0.0);
        assert_eq!(at(MigrationPhase::Reading, 10, 10), 0.25);
        assert_eq!(at(MigrationPhase::Writing, 5, 10), 0.6);
        assert_eq!(at(MigrationPhase::Finishing, 0, 0), 0.95);
    }

    #[test]
    fn unreadable_and_running_sessions_stay_in_place() {
        let legacy = Legacy::new();
        legacy.write("sessions/chat_broken.json", "{ not json");
        legacy.add_session("chat_running", OCT_7);
        let lock =
            file_utils::try_acquire_agent_lock(&legacy.sessions().join("chat_running.agent.lock"))
                .unwrap()
                .unwrap();

        let report = legacy.migrate();
        drop(lock);

        assert!(report.migrated.is_empty());
        assert_eq!(report.skipped.len(), 2);
        assert!(legacy.sessions().join("chat_broken.json").exists());
        assert!(legacy.sessions().join("chat_running.json").exists());
    }
}
