//! Where the Review tab's "Viewed" ticks stay across restarts: one file
//! per repository (by its root), next to the sessions. A tick names the
//! state of the file it was given for, so a later change shows.

use anyhow::Result;
use code_assistant_core::persistence::SessionLayout;
use code_assistant_core::utils::file_utils::atomic_write_json;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a file was when it was ticked: the listing's fingerprint (cheap,
/// but it moves with a touch, a commit or another compare mode) and, once
/// its diff was read, its content (see [`content_token`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewedFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// The ticked files of one repository, by repo-relative path.
pub type ViewedFiles = BTreeMap<String, ViewedFile>;

/// What tells two states of a file apart: a hash of the new side, of the
/// old side for a deleted file. `None` without text (binary, too large).
pub fn content_token(content: &git::FileDiffContent) -> Option<String> {
    if content.is_binary || content.too_large {
        return None;
    }
    let (tag, text) = match (&content.new_text, &content.old_text) {
        (Some(new), _) => ("new", new),
        (None, Some(old)) => ("deleted", old),
        (None, None) => return None,
    };
    let digest = Sha256::new()
        .chain_update(tag)
        .chain_update([0])
        .chain_update(text)
        .finalize();
    Some(digest[..16].iter().map(|b| format!("{b:02x}")).collect())
}

pub trait ReviewedFilesStore: Send + Sync {
    /// The ticks of the repository at `repo_root`; none before the first
    /// save.
    fn load(&self, repo_root: &Path) -> Result<ViewedFiles>;

    /// Store the repository's ticks, replacing the ones before.
    fn save(&self, repo_root: &Path, files: &ViewedFiles) -> Result<()>;
}

#[derive(Serialize, Deserialize)]
struct SavedRepo {
    repo_root: PathBuf,
    files: ViewedFiles,
}

/// The ticks as `reviewed-files/<hash of the root>.json` next to the
/// sessions.
pub struct FileReviewedFilesStore {
    dir: PathBuf,
}

impl FileReviewedFilesStore {
    pub fn new(layout: &SessionLayout) -> Self {
        Self {
            dir: layout.sessions_dir().join("reviewed-files"),
        }
    }

    fn path(&self, repo_root: &Path) -> PathBuf {
        let digest = Sha256::digest(repo_root.to_string_lossy().as_bytes());
        let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        self.dir.join(format!("{name}.json"))
    }
}

impl ReviewedFilesStore for FileReviewedFilesStore {
    fn load(&self, repo_root: &Path) -> Result<ViewedFiles> {
        let json = match std::fs::read_to_string(self.path(repo_root)) {
            Ok(json) => json,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ViewedFiles::new()),
            Err(e) => return Err(e.into()),
        };
        let saved: SavedRepo = serde_json::from_str(&json)?;
        // Another root with the same hash: not ours.
        Ok(match saved.repo_root == repo_root {
            true => saved.files,
            false => ViewedFiles::new(),
        })
    }

    fn save(&self, repo_root: &Path, files: &ViewedFiles) -> Result<()> {
        let path = self.path(repo_root);
        if files.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            };
        }
        atomic_write_json(
            &path,
            &SavedRepo {
                repo_root: repo_root.to_path_buf(),
                files: files.clone(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(old: Option<&str>, new: Option<&str>) -> git::FileDiffContent {
        git::FileDiffContent {
            old_text: old.map(str::to_owned),
            new_text: new.map(str::to_owned),
            is_binary: false,
            too_large: false,
        }
    }

    #[test]
    fn ticks_come_back_per_repository_and_go_with_the_last() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileReviewedFilesStore::new(&SessionLayout::new(dir.path().to_path_buf()));
        let (a, b) = (Path::new("/work/a"), Path::new("/work/b"));
        assert!(store.load(a).unwrap().is_empty());

        let files = ViewedFiles::from([(
            "src/lib.rs".to_owned(),
            ViewedFile {
                fingerprint: Some("x:y".into()),
                content: Some("abc".into()),
            },
        )]);
        store.save(a, &files).unwrap();
        assert_eq!(store.load(a).unwrap(), files);
        assert!(store.load(b).unwrap().is_empty());

        store.save(a, &ViewedFiles::new()).unwrap();
        assert!(store.load(a).unwrap().is_empty());
        assert!(!store.path(a).exists());
    }

    #[test]
    fn the_content_token_follows_the_new_side_or_else_the_old() {
        let token = content_token(&content(Some("a"), Some("b")));
        assert_eq!(token, content_token(&content(Some("other"), Some("b"))));
        assert_ne!(token, content_token(&content(Some("a"), Some("c"))));
        assert_ne!(
            content_token(&content(Some("b"), None)),
            content_token(&content(None, Some("b")))
        );
        let binary = git::FileDiffContent {
            is_binary: true,
            ..content(None, Some("b"))
        };
        assert_eq!(content_token(&binary), None);
    }
}
