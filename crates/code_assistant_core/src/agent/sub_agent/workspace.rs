//! Isolated workspaces for writing sub-agents.
//!
//! A writing sub-agent in a git project runs in its own worktree, started
//! from a snapshot of the parent's tree (uncommitted work included). Its
//! edits, formatter runs, builds and git commands stay there, so several
//! writers can run at once. When it finishes, its changes are merged into
//! the parent's working tree all-or-nothing; on a conflict nothing is
//! applied and the changes stay on a branch for the parent to resolve.

use anyhow::{Context, Result};
use git::isolation::{MergeConflict, changed_paths, merge_into_working_tree, snapshot_working_tree};
use git::{GitBinary, GitRepository, worktree};
use std::path::{Path, PathBuf};

pub(crate) struct ChildWorkspace {
    git: GitBinary,
    /// Root of the parent's working tree.
    repo_root: PathBuf,
    worktree: PathBuf,
    branch: String,
    /// The snapshot of the parent's tree the worktree started from.
    base: String,
    /// The project directory relative to the repository root.
    project_subdir: PathBuf,
}

/// How a finished sub-agent's changes came back.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MergeOutcome {
    NoChanges,
    Merged {
        files: Vec<String>,
    },
    Conflicted {
        conflicts: Vec<MergeConflict>,
        branch: String,
        base: String,
    },
}

/// Whether writing sub-agents for `project` get their own worktree.
pub(crate) fn can_isolate(project: Option<&Path>) -> bool {
    project.is_some_and(GitRepository::is_repo)
}

/// Taken by a writing sub-agent that has to share the project tree (no git
/// repository), so such sub-agents still write one at a time.
pub(crate) async fn shared_tree_writer(project: Option<&Path>) -> tokio::sync::OwnedMutexGuard<()> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    type Slots = Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>;
    static SLOTS: OnceLock<Slots> = OnceLock::new();
    let key = project
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()))
        .unwrap_or_default();
    let slot = SLOTS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(key)
        .or_default()
        .clone();
    slot.lock_owned().await
}

impl ChildWorkspace {
    /// Create a worktree for the sub-agent of `tool_id`. `None` when the
    /// project is not inside a git repository.
    pub async fn create(project: &Path, tool_id: &str) -> Result<Option<Self>> {
        if !GitRepository::is_repo(project) {
            return Ok(None);
        }
        let repo = GitRepository::open(project)?;
        let repo_root = repo.workdir().canonicalize()?;
        let project = project.canonicalize()?;
        let project_subdir = project
            .strip_prefix(&repo_root)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let git = GitBinary::new()?;

        let base =
            snapshot_working_tree(&git, &repo_root, "code-assistant: sub-agent base").await?;
        let id: String = tool_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();
        let branch = format!("code-assistant/sub-agent/{id}-{}", &base[..8]);
        let path = worktree::suggest_worktree_path(&repo_root, &branch);
        let worktree = worktree::create_worktree(&git, &repo_root, &path, &branch, Some(&base))
            .await
            .context("Failed to create a worktree for the sub-agent")?;
        Ok(Some(Self {
            git,
            repo_root,
            worktree,
            branch,
            base,
            project_subdir,
        }))
    }

    /// Root of the worktree, to be allowed by the sandbox.
    pub fn root(&self) -> &Path {
        &self.worktree
    }

    /// Where the sub-agent works: the project directory inside the worktree.
    pub fn project_dir(&self) -> PathBuf {
        self.worktree.join(&self.project_subdir)
    }

    /// Commit what the sub-agent left in the worktree and return the commit
    /// and the paths it changed.
    async fn collect(&self) -> Result<(String, Vec<String>)> {
        let changes = snapshot_working_tree(
            &self.git,
            &self.worktree,
            "code-assistant: sub-agent changes",
        )
        .await?;
        let paths = changed_paths(&self.git, &self.repo_root, &self.base, &changes).await?;
        Ok((changes, paths))
    }

    /// Merge the sub-agent's changes into the parent's working tree.
    pub async fn finish(self) -> Result<MergeOutcome> {
        let (changes, paths) = self.collect().await?;
        if paths.is_empty() {
            self.remove(false).await;
            return Ok(MergeOutcome::NoChanges);
        }
        let result = {
            // The parent and other sub-agents' merges write these files too.
            let _locks =
                fs_explorer::locks::lock_paths(paths.iter().map(|p| self.repo_root.join(p))).await;
            merge_into_working_tree(&self.git, &self.repo_root, &self.base, &changes).await?
        };
        if result.conflicts.is_empty() {
            self.remove(false).await;
            return Ok(MergeOutcome::Merged {
                files: result.applied,
            });
        }
        self.keep_branch_at(&changes).await?;
        let (branch, base) = (self.branch.clone(), self.base.clone());
        self.remove(true).await;
        Ok(MergeOutcome::Conflicted {
            conflicts: result.conflicts,
            branch,
            base,
        })
    }

    /// Drop the worktree of a cancelled or failed sub-agent without applying
    /// anything. Returns the branch its partial changes are kept on, if it
    /// made any.
    pub async fn abandon(self) -> Option<String> {
        let kept = match self.collect().await {
            Ok((changes, paths)) if !paths.is_empty() => {
                self.keep_branch_at(&changes).await.ok().map(|()| self.branch.clone())
            }
            _ => None,
        };
        self.remove(kept.is_some()).await;
        kept
    }

    async fn keep_branch_at(&self, commit: &str) -> Result<()> {
        self.git
            .run(
                &self.repo_root,
                &["update-ref", &format!("refs/heads/{}", self.branch), commit],
            )
            .await?;
        Ok(())
    }

    async fn remove(&self, keep_branch: bool) {
        if let Err(e) =
            worktree::remove_worktree(&self.git, &self.repo_root, &self.worktree, true).await
        {
            tracing::warn!("Failed to remove sub-agent worktree: {e:#}");
        }
        if !keep_branch {
            let _ = self
                .git
                .run(&self.repo_root, &["branch", "-D", &self.branch])
                .await;
        }
    }
}

impl MergeOutcome {
    /// What the parent agent is told, appended to the sub-agent's answer.
    pub fn report(&self) -> Option<String> {
        match self {
            MergeOutcome::NoChanges => None,
            MergeOutcome::Merged { files } => Some(format!(
                "The sub-agent worked in an isolated git worktree. Its changes were merged into your working tree: {}.",
                files.join(", ")
            )),
            MergeOutcome::Conflicted {
                conflicts,
                branch,
                base,
            } => {
                let list: Vec<String> = conflicts
                    .iter()
                    .map(|c| format!("- {} ({})", c.path, c.reason))
                    .collect();
                Some(format!(
                    "The sub-agent worked in an isolated git worktree, but its changes conflict with edits made in your working tree meanwhile, so none of them were applied.\n\
                     Conflicts:\n{}\n\
                     Its complete changes are on branch `{branch}`. Resolve this yourself: review them with `git diff {base} {branch}`, apply them to your working tree by hand, then delete the branch with `git branch -D {branch}`.",
                    list.join("\n")
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("sub/a.txt"), "one\ntwo\nthree\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        dir
    }

    fn branches(repo: &Path) -> String {
        git(repo, &["branch", "--list", "code-assistant/*"])
    }

    #[tokio::test]
    async fn changes_merge_back_and_the_worktree_goes_away() {
        let dir = repo();
        let repo = dir.path().join("repo");
        // Uncommitted parent work is part of the child's starting point.
        std::fs::write(repo.join("sub/draft.txt"), "draft\n").unwrap();

        let ws = ChildWorkspace::create(&repo.join("sub"), "tool_1")
            .await
            .unwrap()
            .unwrap();
        let project = ws.project_dir();
        assert!(project.ends_with("sub"));
        assert_eq!(std::fs::read_to_string(project.join("draft.txt")).unwrap(), "draft\n");
        std::fs::write(project.join("a.txt"), "ONE\ntwo\nthree\n").unwrap();
        let worktree = ws.root().to_path_buf();

        let outcome = ws.finish().await.unwrap();

        assert_eq!(
            outcome,
            MergeOutcome::Merged {
                files: vec!["sub/a.txt".to_string()]
            }
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("sub/a.txt")).unwrap(),
            "ONE\ntwo\nthree\n"
        );
        assert!(!worktree.exists());
        assert_eq!(branches(&repo), "");
    }

    #[tokio::test]
    async fn two_children_editing_one_file_both_land() {
        let dir = repo();
        let repo = dir.path().join("repo");
        let first = ChildWorkspace::create(&repo, "tool_a").await.unwrap().unwrap();
        let second = ChildWorkspace::create(&repo, "tool_b").await.unwrap().unwrap();
        std::fs::write(first.project_dir().join("sub/a.txt"), "ONE\ntwo\nthree\n").unwrap();
        std::fs::write(second.project_dir().join("sub/a.txt"), "one\ntwo\nTHREE\n").unwrap();

        let (a, b) = tokio::join!(first.finish(), second.finish());

        assert!(matches!(a.unwrap(), MergeOutcome::Merged { .. }));
        assert!(matches!(b.unwrap(), MergeOutcome::Merged { .. }));
        assert_eq!(
            std::fs::read_to_string(repo.join("sub/a.txt")).unwrap(),
            "ONE\ntwo\nTHREE\n"
        );
    }

    #[tokio::test]
    async fn a_conflict_keeps_the_branch_and_applies_nothing() {
        let dir = repo();
        let repo = dir.path().join("repo");
        let ws = ChildWorkspace::create(&repo, "tool_c").await.unwrap().unwrap();
        std::fs::write(ws.project_dir().join("sub/a.txt"), "uno\ntwo\nthree\n").unwrap();
        std::fs::write(repo.join("sub/a.txt"), "eins\ntwo\nthree\n").unwrap();

        let outcome = ws.finish().await.unwrap();

        let MergeOutcome::Conflicted { branch, base, .. } = &outcome else {
            panic!("expected a conflict, got {outcome:?}");
        };
        assert_eq!(
            std::fs::read_to_string(repo.join("sub/a.txt")).unwrap(),
            "eins\ntwo\nthree\n"
        );
        assert_eq!(
            git(&repo, &["show", &format!("{branch}:sub/a.txt")]),
            "uno\ntwo\nthree"
        );
        let report = outcome.report().unwrap();
        assert!(report.contains(&format!("git diff {base} {branch}")));
    }

    #[tokio::test]
    async fn abandoning_keeps_partial_changes_on_a_branch_only_if_there_are_any() {
        let dir = repo();
        let repo = dir.path().join("repo");

        let untouched = ChildWorkspace::create(&repo, "tool_d").await.unwrap().unwrap();
        assert_eq!(untouched.abandon().await, None);
        assert_eq!(branches(&repo), "");

        let partial = ChildWorkspace::create(&repo, "tool_e").await.unwrap().unwrap();
        std::fs::write(partial.project_dir().join("sub/a.txt"), "half\n").unwrap();
        let kept = partial.abandon().await.unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.join("sub/a.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
        assert!(branches(&repo).contains(&kept));
    }

    #[tokio::test]
    async fn no_workspace_outside_git() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!can_isolate(Some(dir.path())));
        assert!(
            ChildWorkspace::create(dir.path(), "tool_f")
                .await
                .unwrap()
                .is_none()
        );
    }
}
