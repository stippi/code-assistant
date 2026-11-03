use crate::binary::GitBinary;
use crate::types::{Remote, RepoInfo, Worktree};
use anyhow::{Context, Result};
use gix::ThreadSafeRepository;
use std::path::{Path, PathBuf};

/// A handle to a git repository.
///
/// Uses `gix` for fast in-process operations and the system `git` binary
/// for operations that need hooks (checkout).
pub struct GitRepository {
    /// In-process gix handle for fast reads and branch mutations.
    pub(crate) repo: ThreadSafeRepository,
    /// System git binary for checkout and worktree operations.
    pub git: GitBinary,
    /// Cached working directory path.
    workdir: PathBuf,
}

impl GitRepository {
    /// Open the git repository that contains `path`.
    ///
    /// `path` can be the repo root, a subdirectory, or a worktree directory.
    /// The repository is discovered by walking up the directory tree.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let shared_repo = ThreadSafeRepository::discover_with_environment_overrides_opts(
            path,
            gix::discover::upwards::Options {
                match_ceiling_dir_or_error: false,
                ..Default::default()
            },
            Default::default(),
        )
        .with_context(|| format!("No git repository found at {}", path.display()))?;

        let repo = shared_repo.to_thread_local();
        let workdir = repo
            .workdir()
            .context("Bare repositories are not supported")?
            .to_path_buf();

        let git = GitBinary::new()?;

        Ok(Self {
            repo: shared_repo,
            git,
            workdir,
        })
    }

    /// Check whether a path is inside a git repository.
    pub fn is_repo(path: impl AsRef<Path>) -> bool {
        ThreadSafeRepository::discover(path.as_ref()).is_ok()
    }

    /// Get summary information about this repository.
    pub fn info(&self) -> Result<RepoInfo> {
        let repo = self.repo.to_thread_local();

        let head_branch = repo
            .head_name()
            .ok()
            .flatten()
            .map(|name| name.shorten().to_string());

        let has_origin = repo.remote_names().iter().any(|n| n.as_ref() == b"origin");

        Ok(RepoInfo {
            workdir: self.workdir.clone(),
            git_dir: repo.path().to_path_buf(),
            head_branch,
            has_origin,
        })
    }

    /// The configured remotes in name order, each with the URL it fetches
    /// from (after `insteadOf` rewrites). Remotes without one are left out.
    pub fn remotes(&self) -> Vec<Remote> {
        let repo = self.repo.to_thread_local();
        repo.remote_names()
            .iter()
            .filter_map(|name| {
                let remote = repo.find_remote(name.as_ref()).ok()?;
                let url = remote.url(gix::remote::Direction::Fetch)?;
                Some(Remote {
                    name: name.to_string(),
                    url: url.to_bstring().to_string(),
                })
            })
            .collect()
    }

    /// Return the working directory path.
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Return this worktree's private git directory: `.git` for the main
    /// worktree, `<main>/.git/worktrees/<name>` for a linked one.
    pub fn gitdir(&self) -> PathBuf {
        self.repo.to_thread_local().git_dir().to_path_buf()
    }

    /// Return the common directory (shared across worktrees).
    ///
    /// For the main worktree this is the same as `git_dir`.
    /// For linked worktrees this points to the main repo's `.git` directory.
    pub fn commondir(&self) -> PathBuf {
        self.repo.to_thread_local().common_dir().to_path_buf()
    }

    /// List all worktrees for this repository using gix (no subprocess).
    ///
    /// The main worktree is always first. Linked worktrees follow, sorted by
    /// their git dir path. Worktrees whose checkout path cannot be resolved
    /// are silently skipped.
    pub fn list_worktrees(&self) -> Result<Vec<Worktree>> {
        let repo = self.repo.to_thread_local();
        crate::worktree::list_worktrees_sync(&repo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{init_repo, init_repo_with_commit};
    use tempfile::TempDir;

    #[test]
    fn open_existing_repo() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());

        let repo = GitRepository::open(dir.path()).unwrap();
        // Use canonicalize to handle macOS /var -> /private/var symlink
        let expected = dir.path().canonicalize().unwrap();
        let actual = repo.workdir().canonicalize().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn open_subdirectory() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());

        let sub = dir.path().join("sub").join("deep");
        std::fs::create_dir_all(&sub).unwrap();

        let repo = GitRepository::open(&sub).unwrap();
        let expected = dir.path().canonicalize().unwrap();
        let actual = repo.workdir().canonicalize().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn open_non_repo_fails() {
        let dir = TempDir::new().unwrap();
        assert!(GitRepository::open(dir.path()).is_err());
    }

    #[test]
    fn is_repo_check() {
        let dir = TempDir::new().unwrap();
        assert!(!GitRepository::is_repo(dir.path()));

        init_repo(dir.path());
        assert!(GitRepository::is_repo(dir.path()));
    }

    #[test]
    fn info_on_empty_repo() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());

        let repo = GitRepository::open(dir.path()).unwrap();
        let info = repo.info().unwrap();
        // gix resolves the unborn branch name from config (e.g. "main"), unlike git2
        assert!(info.head_branch.is_some());
        assert!(!info.has_origin);
    }

    #[test]
    fn lists_remotes_in_name_order_with_their_fetch_urls() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        for (name, url) in [
            ("upstream", "git@github.com:team/tool.git"),
            ("origin", "https://github.com/me/tool.git"),
        ] {
            let status = std::process::Command::new("git")
                .args(["remote", "add", name, url])
                .current_dir(dir.path())
                .status()
                .unwrap();
            assert!(status.success());
        }

        let remotes = GitRepository::open(dir.path()).unwrap().remotes();

        let listed: Vec<_> = remotes
            .iter()
            .map(|r| (r.name.as_str(), r.url.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                ("origin", "https://github.com/me/tool.git"),
                ("upstream", "git@github.com:team/tool.git"),
            ]
        );
    }
}
