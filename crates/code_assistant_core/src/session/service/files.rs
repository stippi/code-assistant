//! Read-only access to a session's project files, for a file viewer. The
//! root is the session's effective project path (the worktree when it has
//! one), so the view follows a worktree switch.

use super::*;
use fs_explorer::browse;
pub use fs_explorer::browse::{DirEntry, DirListing, EntryKind, FileContent};
pub use fs_explorer::watch::TreeWatcher;

/// One directory level of a session's project.
#[derive(Debug, Clone)]
pub struct ProjectDir {
    /// The project root the listing is relative to.
    pub root: PathBuf,
    pub listing: DirListing,
}

/// One file of a session's project.
#[derive(Debug, Clone)]
pub struct ProjectFile {
    pub root: PathBuf,
    pub content: FileContent,
}

/// All files of a session's project that gitignore keeps.
#[derive(Debug, Clone)]
pub struct ProjectFiles {
    pub root: PathBuf,
    /// `/`-separated paths relative to `root`.
    pub files: Vec<String>,
    /// The list was cut at [`browse::MAX_LISTED_FILES`].
    pub truncated: bool,
}

impl SessionService {
    /// List the directory `rel_dir` of the session's project (`""` for the
    /// project root).
    pub async fn list_project_dir(
        &self,
        session_id: String,
        rel_dir: PathBuf,
    ) -> Result<ProjectDir> {
        self.with_project_root(session_id, move |root| {
            let listing = browse::list_dir(&root, &rel_dir)?;
            Ok(ProjectDir { root, listing })
        })
        .await
    }

    /// Read the file `rel_path` of the session's project.
    pub async fn read_project_file(
        &self,
        session_id: String,
        rel_path: PathBuf,
    ) -> Result<ProjectFile> {
        self.with_project_root(session_id, move |root| {
            let content = browse::read_file(&root, &rel_path)?;
            Ok(ProjectFile { root, content })
        })
        .await
    }

    /// Replace the content of the existing file `rel_path` of the session's
    /// project (the user saving an edit in a file viewer).
    pub async fn write_project_file(
        &self,
        session_id: String,
        rel_path: PathBuf,
        text: String,
    ) -> Result<()> {
        self.with_project_root(session_id, move |root| {
            browse::write_file(&root, &rel_path, &text)
        })
        .await
    }

    /// List every file of the session's project that gitignore keeps.
    pub async fn list_project_files(&self, session_id: String) -> Result<ProjectFiles> {
        self.with_project_root(session_id, move |root| {
            let (files, truncated) = browse::list_files(&root)?;
            Ok(ProjectFiles {
                root,
                files,
                truncated,
            })
        })
        .await
    }

    /// Run `f` with the session's project root on a blocking thread, so a
    /// large directory never holds up the service.
    async fn with_project_root<T, F>(&self, session_id: String, f: F) -> Result<T>
    where
        F: FnOnce(PathBuf) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        self.call_io(move |ctx| async move {
            let root = {
                let manager = ctx.manager.lock().await;
                session_effective_path(&manager, &session_id)?
            };
            tokio::task::spawn_blocking(move || f(root))
                .await
                .context("Project file access was aborted")?
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_service_with_manager;
    use crate::session::SessionConfig;
    use fs_explorer::browse::{EntryKind, FileContent};
    use std::path::PathBuf;

    #[tokio::test]
    async fn reads_the_sessions_project() {
        let sessions = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join("src")).unwrap();
        std::fs::write(project.path().join("src/lib.rs"), "fn a() {}\n").unwrap();

        let (service, _) = test_service_with_manager(sessions.path());
        let config = SessionConfig {
            init_path: Some(project.path().to_path_buf()),
            ..SessionConfig::default()
        };
        let id = service
            .create_session_with_config(None, config, None)
            .await
            .unwrap();

        let dir = service
            .list_project_dir(id.clone(), PathBuf::new())
            .await
            .unwrap();
        assert_eq!(dir.root, project.path());
        assert_eq!(dir.listing.entries.len(), 1);
        assert_eq!(dir.listing.entries[0].name, "src");
        assert_eq!(dir.listing.entries[0].kind, EntryKind::Dir);

        let file = service
            .read_project_file(id.clone(), PathBuf::from("src/lib.rs"))
            .await
            .unwrap();
        assert_eq!(file.content, FileContent::Text("fn a() {}\n".into()));

        service
            .write_project_file(
                id.clone(),
                PathBuf::from("src/lib.rs"),
                "fn b() {}\n".into(),
            )
            .await
            .unwrap();
        let file = service
            .read_project_file(id.clone(), PathBuf::from("src/lib.rs"))
            .await
            .unwrap();
        assert_eq!(file.content, FileContent::Text("fn b() {}\n".into()));

        let files = service.list_project_files(id.clone()).await.unwrap();
        assert_eq!(files.files, vec!["src/lib.rs".to_string()]);

        assert!(
            service
                .read_project_file(id, PathBuf::from("../escape"))
                .await
                .is_err()
        );
    }
}
