use crate::binary::GitBinary;
use crate::types::RepoInfo;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// A handle to a git repository.
///
/// Uses `git2` (libgit2) for fast in-process reads and the system `git`
/// binary for write operations, worktree management, and anything that
/// may need hooks or credential helpers.
pub struct GitRepository {
    /// In-process libgit2 handle for fast reads.
    pub(crate) repo: git2::Repository,
    /// System git binary for CLI operations.
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
        let repo = git2::Repository::discover(path)
            .with_context(|| format!("No git repository found at {}", path.display()))?;

        let workdir = repo
            .workdir()
            .context("Bare repositories are not supported")?
            .to_path_buf();

        let git = GitBinary::new()?;

        Ok(Self { repo, git, workdir })
    }

    /// Check whether a path is inside a git repository.
    pub fn is_repo(path: impl AsRef<Path>) -> bool {
        git2::Repository::discover(path.as_ref()).is_ok()
    }

    /// Get summary information about this repository.
    pub fn info(&self) -> Result<RepoInfo> {
        let head_branch = self.repo.head().ok().and_then(|head| {
            if head.is_branch() {
                head.shorthand().map(String::from)
            } else {
                None
            }
        });

        let has_origin = self.repo.find_remote("origin").is_ok();

        Ok(RepoInfo {
            workdir: self.workdir.clone(),
            git_dir: self.repo.path().to_path_buf(),
            head_branch,
            has_origin,
        })
    }

    /// Return the working directory path.
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Return the common directory (shared across worktrees).
    ///
    /// For the main worktree this is the same as `git_dir`.
    /// For linked worktrees this points to the main repo's `.git` directory.
    pub fn commondir(&self) -> PathBuf {
        self.repo.commondir().to_path_buf()
    }

    /// Provide read access to the underlying `git2::Repository`.
    ///
    /// Useful for callers that need direct libgit2 access
    /// (e.g. for computing in-memory diffs).
    pub fn libgit2(&self) -> &git2::Repository {
        &self.repo
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_repo(dir: &Path) -> git2::Repository {
        git2::Repository::init(dir).expect("failed to init repo")
    }

    #[test]
    fn open_existing_repo() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());

        let repo = GitRepository::open(dir.path()).unwrap();
        // Use canonicalize to handle macOS /var -> /private/var symlink
        let expected = dir.path().canonicalize().unwrap();
        assert_eq!(repo.workdir(), expected);
    }

    #[test]
    fn open_subdirectory() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());

        let sub = dir.path().join("sub").join("deep");
        std::fs::create_dir_all(&sub).unwrap();

        let repo = GitRepository::open(&sub).unwrap();
        let expected = dir.path().canonicalize().unwrap();
        assert_eq!(repo.workdir(), expected);
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
        init_repo(dir.path());

        let repo = GitRepository::open(dir.path()).unwrap();
        let info = repo.info().unwrap();
        // HEAD is unborn on an empty repo
        assert_eq!(info.head_branch, None);
        assert!(!info.has_origin);
    }
}
