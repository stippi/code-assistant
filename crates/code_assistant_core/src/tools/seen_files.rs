//! The file contents an agent has seen.
//!
//! Several agents can work in one tree: the main agent, its sub-agents, and
//! the user in an editor. An agent that changes a file based on content it
//! read earlier must not write over changes it never saw. So every agent
//! remembers the version of each file it last read or wrote, and a change to
//! a file that differs from that version fails: the agent reads the file
//! again and decides what to do. Files it never saw it may change.

use anyhow::{Result, bail};
use fs_explorer::CodeExplorer;
use fs_explorer::locks::{lock_path, path_key};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The version of each file one agent last read or wrote.
#[derive(Debug, Default)]
pub struct SeenFiles {
    versions: Mutex<HashMap<PathBuf, String>>,
}

fn version(content: &str) -> String {
    format!("{:x}", md5::compute(content))
}

impl SeenFiles {
    /// Note that the agent saw `content` as the whole of the file at `path`.
    pub fn saw(&self, path: &Path, content: &str) {
        self.versions
            .lock()
            .unwrap()
            .insert(path_key(path), version(content));
    }

    fn forget(&self, path: &Path) {
        self.versions.lock().unwrap().remove(&path_key(path));
    }

    fn version_seen(&self, path: &Path) -> Option<String> {
        self.versions.lock().unwrap().get(&path_key(path)).cloned()
    }

    /// Run `change` on the file at `path`, unless the file changed since the
    /// agent last saw it. Other writers of the path wait meanwhile, so the
    /// check and the change are one step. Afterwards the file as `change`
    /// left it counts as seen, if it succeeded.
    pub async fn change<T>(
        &self,
        explorer: &dyn CodeExplorer,
        path: &Path,
        change: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let _lock = lock_path(path).await;
        if let Some(seen) = self.version_seen(path) {
            let current = explorer.read_file(path).await.ok();
            if current.as_deref().map(version) != Some(seen) {
                let what = if current.is_some() {
                    "changed"
                } else {
                    "deleted"
                };
                bail!(
                    "{} was {what} since you last read it, by another agent, a command or the user. \
                     Read it again and decide how to go on from its current content.",
                    path.display()
                );
            }
        }
        let result = change.await;
        if result.is_ok() {
            match explorer.read_file(path).await {
                Ok(content) => self.saw(path, &content),
                Err(_) => self.forget(path),
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_explorer::{CodeExplorer, Explorer, FileReplacement};
    use std::path::Path;

    fn replacement(search: &str, replace: &str) -> Vec<FileReplacement> {
        vec![FileReplacement {
            search: search.to_string(),
            replace: replace.to_string(),
            replace_all: false,
        }]
    }

    async fn setup() -> (tempfile::TempDir, Explorer, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "one\ntwo\n").unwrap();
        let explorer = Explorer::new(dir.path().to_path_buf());
        (dir, explorer, file)
    }

    async fn edit(
        seen: &SeenFiles,
        explorer: &Explorer,
        file: &Path,
        search: &str,
        replace: &str,
    ) -> anyhow::Result<String> {
        seen.change(
            explorer,
            file,
            explorer.apply_replacements(file, &replacement(search, replace)),
        )
        .await
    }

    #[tokio::test]
    async fn a_file_never_seen_can_be_changed() {
        let (_dir, explorer, file) = setup().await;
        let seen = SeenFiles::default();

        edit(&seen, &explorer, &file, "one", "ONE").await.unwrap();

        assert_eq!(std::fs::read_to_string(&file).unwrap(), "ONE\ntwo\n");
    }

    #[tokio::test]
    async fn a_file_changed_since_it_was_read_is_not_changed() {
        let (_dir, explorer, file) = setup().await;
        let seen = SeenFiles::default();
        seen.saw(&file, &explorer.read_file(&file).await.unwrap());
        // Someone else changes it meanwhile.
        std::fs::write(&file, "one\nTWO\n").unwrap();

        let error = edit(&seen, &explorer, &file, "one", "ONE")
            .await
            .unwrap_err();

        assert!(
            error.to_string().contains("changed since you last read it"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "one\nTWO\n");
        // Read again, the change goes through.
        seen.saw(&file, &explorer.read_file(&file).await.unwrap());
        edit(&seen, &explorer, &file, "one", "ONE").await.unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "ONE\nTWO\n");
    }

    #[tokio::test]
    async fn an_agents_own_changes_count_as_seen() {
        let (_dir, explorer, file) = setup().await;
        let seen = SeenFiles::default();
        seen.saw(&file, &explorer.read_file(&file).await.unwrap());

        edit(&seen, &explorer, &file, "one", "ONE").await.unwrap();
        edit(&seen, &explorer, &file, "two", "TWO").await.unwrap();

        assert_eq!(std::fs::read_to_string(&file).unwrap(), "ONE\nTWO\n");
    }

    #[tokio::test]
    async fn a_file_deleted_since_it_was_read_is_not_written() {
        let (_dir, explorer, file) = setup().await;
        let seen = SeenFiles::default();
        seen.saw(&file, &explorer.read_file(&file).await.unwrap());
        std::fs::remove_file(&file).unwrap();

        let result = seen
            .change(&explorer, &file, explorer.write_file(&file, "new\n", false))
            .await;

        assert!(result.is_err());
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn of_two_agents_that_read_a_file_only_the_first_writes() {
        let (_dir, explorer, file) = setup().await;
        let (first, second) = (SeenFiles::default(), SeenFiles::default());
        let content = explorer.read_file(&file).await.unwrap();
        first.saw(&file, &content);
        second.saw(&file, &content);

        let (a, b) = tokio::join!(
            edit(&first, &explorer, &file, "one", "ONE"),
            edit(&second, &explorer, &file, "two", "TWO"),
        );

        assert_eq!(
            [a.is_ok(), b.is_ok()].iter().filter(|ok| **ok).count(),
            1,
            "exactly one of {a:?} / {b:?} succeeds"
        );
    }
}
