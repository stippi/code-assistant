//! Where a session's files live on disk, and how session IDs are made.
//!
//! Every session has a folder below the sessions directory; the session ID
//! is that folder's path relative to it:
//!
//! ```text
//! sessions/
//!   metadata.json, lifecycle.json
//!   -Users-me-workspace-code-assistant/
//!     2026-10-07-001/          ← id "-Users-me-workspace-code-assistant/2026-10-07-001"
//!       session.json
//!       blobs/<sha256>.json     large tool results
//!       entry.lock, agent.lock
//!       ui_state.json, draft.json
//! ```
//!
//! Generated IDs are `<project-slug>/<YYYY-MM-DD>-<NNN>`, so the folder tells
//! which project a session belongs to and when it was started. IDs supplied
//! from elsewhere (tests, embedders) may be any relative path of plain
//! components; see [`validate_session_id`].

use anyhow::{Result, bail};
use chrono::NaiveDate;
use std::path::{Component, Path, PathBuf};

/// The slug grouping the sessions of projects that have none.
pub const NO_PROJECT_SLUG: &str = "_no-project";

/// The session record inside a session folder.
const SESSION_FILE: &str = "session.json";
const ENTRY_LOCK_FILE: &str = "entry.lock";
const AGENT_LOCK_FILE: &str = "agent.lock";
const UI_STATE_FILE: &str = "ui_state.json";
const DRAFT_FILE: &str = "draft.json";
const BLOBS_DIR: &str = "blobs";

/// What a changed path below the sessions directory means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionPath {
    /// `metadata.json` or `lifecycle.json`: the session list changed.
    Index,
    /// A session's record changed.
    Record(String),
    /// A session's agent lock appeared or disappeared.
    AgentLock(String),
}

/// Paths of the session store, rooted at the sessions directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLayout {
    sessions_dir: PathBuf,
}

impl SessionLayout {
    pub fn new(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    /// The folder of a session. Fails for IDs that are not a relative path
    /// of plain components.
    pub fn session_dir(&self, session_id: &str) -> Result<PathBuf> {
        validate_session_id(session_id)?;
        Ok(self.sessions_dir.join(session_id))
    }

    pub fn session_file(&self, session_id: &str) -> Result<PathBuf> {
        Ok(self.session_dir(session_id)?.join(SESSION_FILE))
    }

    pub fn entry_lock(&self, session_id: &str) -> Result<PathBuf> {
        Ok(self.session_dir(session_id)?.join(ENTRY_LOCK_FILE))
    }

    pub fn agent_lock(&self, session_id: &str) -> Result<PathBuf> {
        Ok(self.session_dir(session_id)?.join(AGENT_LOCK_FILE))
    }

    pub fn ui_state(&self, session_id: &str) -> Result<PathBuf> {
        Ok(self.session_dir(session_id)?.join(UI_STATE_FILE))
    }

    pub fn draft(&self, session_id: &str) -> Result<PathBuf> {
        Ok(self.session_dir(session_id)?.join(DRAFT_FILE))
    }

    /// Externalized tool results (see `persistence::blobs`).
    pub fn blobs_dir(&self, session_id: &str) -> Result<PathBuf> {
        Ok(self.session_dir(session_id)?.join(BLOBS_DIR))
    }

    /// Reserve a new session ID for a project and create its folder.
    ///
    /// The counter continues after the highest number of that project and
    /// day. Creating the folder is the reservation: if another process got
    /// there first, the next number is taken, so no lock is needed.
    pub fn allocate_session_id(
        &self,
        project_root: Option<&Path>,
        date: NaiveDate,
    ) -> Result<String> {
        let slug = project_slug(project_root);
        let project_dir = self.sessions_dir.join(&slug);
        std::fs::create_dir_all(&project_dir)?;

        let prefix = format!("{}-", date.format("%Y-%m-%d"));
        let highest = std::fs::read_dir(&project_dir)?
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                name.to_str()?.strip_prefix(&prefix)?.parse::<u32>().ok()
            })
            .max()
            .unwrap_or(0);

        let mut counter = highest + 1;
        loop {
            let session_id = format!("{slug}/{prefix}{counter:03}");
            match std::fs::create_dir(self.sessions_dir.join(&session_id)) {
                Ok(()) => return Ok(session_id),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => counter += 1,
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// The IDs of all sessions that have a record, found by walking the
    /// folders (IDs are at most two components deep).
    pub fn session_ids(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for top in read_dirs(&self.sessions_dir)? {
            let top_name = file_name(&top);
            if top.join(SESSION_FILE).exists() {
                ids.push(top_name.clone());
            }
            for nested in read_dirs(&top)? {
                if nested.join(SESSION_FILE).exists() {
                    ids.push(format!("{top_name}/{}", file_name(&nested)));
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Classify a path reported by a filesystem watcher. `None` for paths
    /// that don't matter to anyone watching sessions (UI state, drafts,
    /// temp files, other files).
    pub fn classify(&self, path: &Path) -> Option<SessionPath> {
        let relative = path.strip_prefix(&self.sessions_dir).ok()?;
        let file = relative.file_name()?.to_str()?;
        let parent = relative.parent()?;
        if parent.as_os_str().is_empty() {
            return matches!(file, "metadata.json" | "lifecycle.json")
                .then_some(SessionPath::Index);
        }
        let session_id = parent.to_str()?.replace(std::path::MAIN_SEPARATOR, "/");
        validate_session_id(&session_id).ok()?;
        match file {
            SESSION_FILE => Some(SessionPath::Record(session_id)),
            AGENT_LOCK_FILE => Some(SessionPath::AgentLock(session_id)),
            _ => None,
        }
    }
}

fn read_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    match std::fs::read_dir(dir) {
        Ok(entries) => Ok(entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The folder name grouping a project's sessions: the project root with
/// every character other than ASCII letters, digits, `_` and `-` replaced
/// by `-`, as Claude Code names its project folders. Lossy on purpose; the
/// full path stays in the session record.
pub fn project_slug(project_root: Option<&Path>) -> String {
    let Some(root) = project_root.filter(|root| !root.as_os_str().is_empty()) else {
        return NO_PROJECT_SLUG.to_string();
    };
    root.to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Session IDs become paths, so they must stay inside the sessions
/// directory: one or two `/`-separated plain components, no `.` or `..`,
/// nothing absolute, no backslashes.
pub fn validate_session_id(session_id: &str) -> Result<()> {
    let components: Vec<&str> = session_id.split('/').collect();
    let plain = |part: &&str| {
        !part.is_empty()
            && !part.contains('\\')
            && matches!(
                Path::new(part).components().collect::<Vec<_>>().as_slice(),
                [Component::Normal(_)]
            )
    };
    if components.len() > 2 || !components.iter().all(plain) {
        bail!("Invalid session id: {session_id:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, d).unwrap()
    }

    #[test]
    fn slug_follows_the_project_root() {
        assert_eq!(
            project_slug(Some(Path::new("/Users/me/workspace/code-assistant"))),
            "-Users-me-workspace-code-assistant"
        );
        assert_eq!(project_slug(Some(Path::new("/tmp/a b.c"))), "-tmp-a-b-c");
        assert_eq!(project_slug(None), NO_PROJECT_SLUG);
        assert_eq!(project_slug(Some(Path::new(""))), NO_PROJECT_SLUG);
    }

    #[test]
    fn session_ids_must_stay_inside_the_sessions_directory() {
        for valid in ["s", "project/2026-10-07-001", "-Users-me/x"] {
            assert!(validate_session_id(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "", "/abs", "a/", "/", "..", "a/..", "../a", ".", "a/./b", "a/b/c", "a\\b",
        ] {
            assert!(validate_session_id(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn allocation_counts_per_project_and_day() {
        let dir = tempdir().unwrap();
        let layout = SessionLayout::new(dir.path().to_path_buf());
        let project = Some(Path::new("/w/p"));

        assert_eq!(
            layout.allocate_session_id(project, day(7)).unwrap(),
            "-w-p/2026-10-07-001"
        );
        assert_eq!(
            layout.allocate_session_id(project, day(7)).unwrap(),
            "-w-p/2026-10-07-002"
        );
        assert_eq!(
            layout.allocate_session_id(project, day(8)).unwrap(),
            "-w-p/2026-10-08-001"
        );
        assert_eq!(
            layout.allocate_session_id(None, day(7)).unwrap(),
            "_no-project/2026-10-07-001"
        );
        assert!(dir.path().join("-w-p/2026-10-07-002").is_dir());
    }

    #[test]
    fn allocation_continues_after_the_highest_number() {
        let dir = tempdir().unwrap();
        let layout = SessionLayout::new(dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path().join("-w-p/2026-10-07-041")).unwrap();
        std::fs::create_dir_all(dir.path().join("-w-p/2026-10-07-003")).unwrap();

        assert_eq!(
            layout
                .allocate_session_id(Some(Path::new("/w/p")), day(7))
                .unwrap(),
            "-w-p/2026-10-07-042"
        );
    }

    #[test]
    fn concurrent_allocations_never_share_an_id() {
        let dir = tempdir().unwrap();
        let layout = SessionLayout::new(dir.path().to_path_buf());
        let mut ids: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let layout = layout.clone();
                    scope.spawn(move || {
                        layout
                            .allocate_session_id(Some(Path::new("/w/p")), day(7))
                            .unwrap()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 8);
    }

    #[test]
    fn session_ids_are_found_one_and_two_levels_deep() {
        let dir = tempdir().unwrap();
        let layout = SessionLayout::new(dir.path().to_path_buf());
        for id in ["flat", "p/2026-10-07-001", "p/2026-10-07-002"] {
            std::fs::create_dir_all(layout.session_dir(id).unwrap()).unwrap();
            std::fs::write(layout.session_file(id).unwrap(), "{}").unwrap();
        }
        // A reserved folder without a record is not a session.
        std::fs::create_dir_all(layout.session_dir("p/2026-10-07-003").unwrap()).unwrap();

        assert_eq!(
            layout.session_ids().unwrap(),
            ["flat", "p/2026-10-07-001", "p/2026-10-07-002"]
        );
    }

    #[test]
    fn watched_paths_map_back_to_sessions() {
        let layout = SessionLayout::new(PathBuf::from("/data/sessions"));
        let classify = |p: &str| layout.classify(Path::new(p));

        assert_eq!(
            classify("/data/sessions/metadata.json"),
            Some(SessionPath::Index)
        );
        assert_eq!(
            classify("/data/sessions/lifecycle.json"),
            Some(SessionPath::Index)
        );
        assert_eq!(
            classify("/data/sessions/p/2026-10-07-001/session.json"),
            Some(SessionPath::Record("p/2026-10-07-001".into()))
        );
        assert_eq!(
            classify("/data/sessions/p/2026-10-07-001/agent.lock"),
            Some(SessionPath::AgentLock("p/2026-10-07-001".into()))
        );
        assert_eq!(
            classify("/data/sessions/flat/session.json"),
            Some(SessionPath::Record("flat".into()))
        );
        assert_eq!(
            classify("/data/sessions/p/2026-10-07-001/ui_state.json"),
            None
        );
        assert_eq!(classify("/data/sessions/p/2026-10-07-001/.tmpX1"), None);
        assert_eq!(classify("/data/sessions/other.json"), None);
        assert_eq!(classify("/elsewhere/metadata.json"), None);
    }
}
