use crate::repository::GitRepository;
use crate::types::Worktree;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

impl GitRepository {
    /// List all worktrees for this repository.
    ///
    /// Parses `git worktree list --porcelain` output. Always includes
    /// the main worktree as the first entry.
    pub async fn list_worktrees(&self) -> Result<Vec<Worktree>> {
        let output = self
            .git
            .run(
                self.workdir(),
                &["--no-optional-locks", "worktree", "list", "--porcelain"],
            )
            .await?;

        Ok(parse_worktrees(&output))
    }

    /// Create a new worktree.
    ///
    /// Creates a new worktree at `path` with a new branch `branch_name`
    /// based on `base` (a branch name, tag, or commit SHA). If `base`
    /// is `None`, HEAD is used.
    ///
    /// Returns the absolute path to the created worktree.
    pub async fn create_worktree(
        &self,
        path: &Path,
        branch_name: &str,
        base: Option<&str>,
    ) -> Result<PathBuf> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent dir {}", parent.display()))?;
        }

        let base_ref = base.unwrap_or("HEAD");
        let path_str = path.to_str().context("Worktree path must be valid UTF-8")?;

        self.git
            .run(
                self.workdir(),
                &[
                    "--no-optional-locks",
                    "worktree",
                    "add",
                    "-b",
                    branch_name,
                    "--",
                    path_str,
                    base_ref,
                ],
            )
            .await?;

        // Return the canonical path
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        Ok(canonical)
    }

    /// Create a worktree for an existing branch (no `-b` flag).
    ///
    /// Checks out `branch_name` into the directory at `path`.
    pub async fn create_worktree_for_branch(
        &self,
        path: &Path,
        branch_name: &str,
    ) -> Result<PathBuf> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent dir {}", parent.display()))?;
        }

        let path_str = path.to_str().context("Worktree path must be valid UTF-8")?;

        self.git
            .run(
                self.workdir(),
                &[
                    "--no-optional-locks",
                    "worktree",
                    "add",
                    "--",
                    path_str,
                    branch_name,
                ],
            )
            .await?;

        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        Ok(canonical)
    }

    /// Remove a worktree. Set `force` to remove even with uncommitted changes.
    pub async fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        let path_str = path.to_str().context("Worktree path must be valid UTF-8")?;

        let mut args = vec!["--no-optional-locks", "worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push("--");
        args.push(path_str);

        self.git.run(self.workdir(), &args).await?;
        Ok(())
    }

    /// Find the worktree that has the given branch checked out, if any.
    pub async fn find_worktree_for_branch(&self, branch_name: &str) -> Result<Option<Worktree>> {
        let worktrees = self.list_worktrees().await?;
        let full_ref = format!("refs/heads/{branch_name}");
        Ok(worktrees
            .into_iter()
            .find(|wt| wt.branch.as_deref() == Some(&full_ref)))
    }

    /// Suggest a worktree path for a given branch name.
    ///
    /// Convention: `<parent_of_repo>/.worktrees/<repo_name>/<sanitized_branch>`
    pub fn suggest_worktree_path(&self, branch_name: &str) -> PathBuf {
        let repo_name = self
            .workdir()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repo");

        let sanitized_branch = branch_name.replace('/', "-");

        let parent = self.workdir().parent().unwrap_or(self.workdir());

        parent
            .join(".worktrees")
            .join(repo_name)
            .join(sanitized_branch)
    }
}

/// Parse `git worktree list --porcelain` output into structured data.
///
/// The porcelain format emits entries separated by blank lines:
/// ```text
/// worktree /path/to/main
/// HEAD abc123...
/// branch refs/heads/main
///
/// worktree /path/to/feature
/// HEAD def456...
/// branch refs/heads/feature
/// ```
fn parse_worktrees(output: &str) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    let normalized = output.replace("\r\n", "\n");

    for (idx, entry) in normalized.split("\n\n").enumerate() {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }

        let mut path = None;
        let mut head_sha = None;
        let mut branch = None;
        let mut is_bare = false;

        for line in entry.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(rest));
            } else if let Some(rest) = line.strip_prefix("HEAD ") {
                head_sha = Some(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("branch ") {
                branch = Some(rest.to_string());
            } else if line == "bare" {
                is_bare = true;
            }
        }

        if is_bare {
            continue;
        }

        if let (Some(path), Some(head_sha)) = (path, head_sha) {
            worktrees.push(Worktree {
                path,
                branch,
                head_sha,
                is_main: idx == 0,
            });
        }
    }

    worktrees
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_main_worktree() {
        let input = "\
worktree /home/user/project
HEAD abc123def456
branch refs/heads/main
";
        let wts = parse_worktrees(input);
        assert_eq!(wts.len(), 1);
        assert_eq!(wts[0].path, PathBuf::from("/home/user/project"));
        assert_eq!(wts[0].head_sha, "abc123def456");
        assert_eq!(wts[0].branch.as_deref(), Some("refs/heads/main"));
        assert_eq!(wts[0].branch_name(), Some("main"));
        assert!(wts[0].is_main);
    }

    #[test]
    fn parse_multiple_worktrees() {
        let input = "\
worktree /home/user/project
HEAD abc123
branch refs/heads/main

worktree /home/user/.worktrees/project/feature-x
HEAD def456
branch refs/heads/feature/x
";
        let wts = parse_worktrees(input);
        assert_eq!(wts.len(), 2);

        assert!(wts[0].is_main);
        assert_eq!(wts[0].branch_name(), Some("main"));

        assert!(!wts[1].is_main);
        assert_eq!(wts[1].branch_name(), Some("feature/x"));
        assert_eq!(
            wts[1].path,
            PathBuf::from("/home/user/.worktrees/project/feature-x")
        );
    }

    #[test]
    fn parse_detached_head() {
        let input = "\
worktree /home/user/project
HEAD abc123
branch refs/heads/main

worktree /tmp/detached
HEAD 789abc
detached
";
        let wts = parse_worktrees(input);
        assert_eq!(wts.len(), 2);
        assert!(wts[1].branch.is_none());
        assert_eq!(wts[1].branch_name(), None);
    }

    #[test]
    fn parse_bare_repo_skipped() {
        let input = "\
worktree /home/user/project.git
HEAD abc123
bare
";
        let wts = parse_worktrees(input);
        assert_eq!(wts.len(), 0);
    }

    #[test]
    fn parse_empty_input() {
        let wts = parse_worktrees("");
        assert!(wts.is_empty());
    }

    #[test]
    fn parse_crlf_line_endings() {
        let input = "worktree /home/user/project\r\nHEAD abc123\r\nbranch refs/heads/main\r\n";
        let wts = parse_worktrees(input);
        assert_eq!(wts.len(), 1);
        assert_eq!(wts[0].branch_name(), Some("main"));
    }

    #[test]
    fn suggest_worktree_path_simple() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo_dir = dir.path().join("my-project");
        std::fs::create_dir_all(&repo_dir).unwrap();
        git2::Repository::init(&repo_dir).unwrap();

        let repo = GitRepository::open(&repo_dir).unwrap();
        let suggested = repo.suggest_worktree_path("feature/login");

        // Use canonicalize to handle macOS /var -> /private/var symlink
        let expected = dir
            .path()
            .canonicalize()
            .unwrap()
            .join(".worktrees")
            .join("my-project")
            .join("feature-login");

        assert_eq!(suggested, expected);
    }

    // Integration tests that require git binary

    #[tokio::test]
    async fn create_and_list_worktree() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo_dir = dir.path().join("main-repo");
        std::fs::create_dir_all(&repo_dir).unwrap();

        // Need a commit before we can create worktrees
        let git_repo = git2::Repository::init(&repo_dir).unwrap();
        {
            let sig = git2::Signature::now("Test", "test@test.com").unwrap();
            let tree_id = {
                let mut index = git_repo.index().unwrap();
                let file_path = repo_dir.join("README.md");
                std::fs::write(&file_path, "# Test\n").unwrap();
                index.add_path(std::path::Path::new("README.md")).unwrap();
                index.write().unwrap();
                index.write_tree().unwrap()
            };
            let tree = git_repo.find_tree(tree_id).unwrap();
            git_repo
                .commit(Some("HEAD"), &sig, &sig, "Initial", &tree, &[])
                .unwrap();
        }
        drop(git_repo);

        let repo = GitRepository::open(&repo_dir).unwrap();

        // Create a worktree
        let wt_path = dir.path().join("worktree-feature");
        repo.create_worktree(&wt_path, "feature-branch", None)
            .await
            .unwrap();

        assert!(wt_path.exists());

        // List worktrees
        let worktrees = repo.list_worktrees().await.unwrap();
        assert_eq!(worktrees.len(), 2);
        assert!(worktrees[0].is_main);
        assert!(!worktrees[1].is_main);
        assert_eq!(worktrees[1].branch_name(), Some("feature-branch"));

        // Find worktree for branch
        let found = repo
            .find_worktree_for_branch("feature-branch")
            .await
            .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().branch_name(), Some("feature-branch"));

        let not_found = repo.find_worktree_for_branch("nonexistent").await.unwrap();
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn create_and_remove_worktree() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo_dir = dir.path().join("main-repo");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let git_repo = git2::Repository::init(&repo_dir).unwrap();
        {
            let sig = git2::Signature::now("Test", "test@test.com").unwrap();
            let tree_id = {
                let mut index = git_repo.index().unwrap();
                let file_path = repo_dir.join("README.md");
                std::fs::write(&file_path, "# Test\n").unwrap();
                index.add_path(std::path::Path::new("README.md")).unwrap();
                index.write().unwrap();
                index.write_tree().unwrap()
            };
            let tree = git_repo.find_tree(tree_id).unwrap();
            git_repo
                .commit(Some("HEAD"), &sig, &sig, "Initial", &tree, &[])
                .unwrap();
        }
        drop(git_repo);

        let repo = GitRepository::open(&repo_dir).unwrap();

        let wt_path = dir.path().join("to-remove");
        repo.create_worktree(&wt_path, "temp-branch", None)
            .await
            .unwrap();
        assert!(wt_path.exists());

        repo.remove_worktree(&wt_path, false).await.unwrap();
        assert!(!wt_path.exists());

        let worktrees = repo.list_worktrees().await.unwrap();
        assert_eq!(worktrees.len(), 1); // only main remains
    }

    #[tokio::test]
    async fn create_worktree_for_existing_branch() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo_dir = dir.path().join("main-repo");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let git_repo = git2::Repository::init(&repo_dir).unwrap();
        {
            let sig = git2::Signature::now("Test", "test@test.com").unwrap();
            let tree_id = {
                let mut index = git_repo.index().unwrap();
                let file_path = repo_dir.join("README.md");
                std::fs::write(&file_path, "# Test\n").unwrap();
                index.add_path(std::path::Path::new("README.md")).unwrap();
                index.write().unwrap();
                index.write_tree().unwrap()
            };
            let tree = git_repo.find_tree(tree_id).unwrap();
            let commit = git_repo
                .commit(Some("HEAD"), &sig, &sig, "Initial", &tree, &[])
                .unwrap();
            let commit = git_repo.find_commit(commit).unwrap();
            git_repo.branch("existing-branch", &commit, false).unwrap();
        }
        drop(git_repo);

        let repo = GitRepository::open(&repo_dir).unwrap();

        let wt_path = dir.path().join("existing-wt");
        repo.create_worktree_for_branch(&wt_path, "existing-branch")
            .await
            .unwrap();
        assert!(wt_path.exists());

        let worktrees = repo.list_worktrees().await.unwrap();
        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[1].branch_name(), Some("existing-branch"));
    }
}
