use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A local git branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    /// Branch name without `refs/heads/` prefix (e.g. `main`, `feature/foo`).
    pub name: String,
    /// Whether this is the currently checked-out branch.
    pub is_head: bool,
    /// The upstream tracking branch, if any (e.g. `origin/main`).
    pub upstream: Option<String>,
}

/// A git worktree entry as reported by `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    /// Absolute path to the worktree directory.
    pub path: PathBuf,
    /// The branch checked out in this worktree (e.g. `refs/heads/main`).
    /// `None` if the worktree is in detached HEAD state.
    pub branch: Option<String>,
    /// The HEAD commit SHA.
    pub head_sha: String,
    /// Whether this is the main worktree (the original clone).
    pub is_main: bool,
}

impl Worktree {
    /// Returns the short branch name (without `refs/heads/` prefix).
    pub fn branch_name(&self) -> Option<&str> {
        self.branch
            .as_deref()
            .and_then(|b| b.strip_prefix("refs/heads/"))
    }
}

/// Summary information about a git repository.
#[derive(Debug, Clone)]
pub struct RepoInfo {
    /// Path to the working directory (the directory containing `.git`).
    pub workdir: PathBuf,
    /// Path to the `.git` directory itself.
    pub git_dir: PathBuf,
    /// The current branch name, if on a branch.
    pub head_branch: Option<String>,
    /// Whether the repository has a remote named `origin`.
    pub has_origin: bool,
}
