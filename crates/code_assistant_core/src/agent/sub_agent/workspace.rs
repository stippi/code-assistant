//! Worktrees for sub-agents.
//!
//! A sub-agent in a git project can work in a worktree of its own, on a
//! branch of its own, started from a snapshot of the parent's tree
//! (uncommitted and untracked work included). With [`Fill::Copy`] the
//! worktree holds a copy-on-write clone of everything in the parent's tree,
//! ignored files included ([`super::clone_tree`]), so build outputs and
//! installed dependencies come along; where the file system cannot clone
//! files, and with [`Fill::Checkout`], it is a checkout of the snapshot.
//!
//! Nothing flows back into the parent's tree by itself. When the sub-agent
//! ends, what it changed is committed to its branch, the worktree goes away
//! and the parent is told how to review and apply the changes
//! ([`KeptChanges`]).
//!
//! Every worktree is locked (`git worktree lock`) with the id of the process
//! that runs it. The next workspace created in the repository removes those
//! of processes that are gone, keeping their changes on their branches.

use super::clone_tree::clone_tree;
use anyhow::{Context, Result};
use git::isolation::{changed_paths, snapshot_working_tree};
use git::{GitBinary, GitRepository, worktree};
use std::path::{Path, PathBuf};

/// Branches of sub-agent worktrees start with this.
const BRANCH_PREFIX: &str = "code-assistant/sub-agent/";
/// Lock reasons of sub-agent worktrees start with this.
const LOCK_PREFIX: &str = "code-assistant sub-agent";

/// What a sub-agent's worktree is filled with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fill {
    /// Everything in the parent's tree, ignored files included.
    Copy,
    /// What git tracks in the parent's tree, without ignored files.
    Checkout,
}

pub(crate) struct ChildWorkspace {
    git: GitBinary,
    /// Root of the parent's working tree.
    repo_root: PathBuf,
    worktree: PathBuf,
    branch: String,
    /// The commit the worktree's files started from.
    base: String,
    /// The project directory relative to the repository root.
    project_subdir: PathBuf,
}

/// What a sub-agent changed, kept on a branch of the parent's repository.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct KeptChanges {
    pub repo_root: PathBuf,
    pub branch: String,
    /// The commit the changes are measured against.
    pub base: String,
    /// Changed paths, relative to the repository root.
    pub files: Vec<String>,
}

impl ChildWorkspace {
    /// Create a worktree for the sub-agent of `tool_id`. `None` when the
    /// project is not inside a git repository.
    pub async fn create(project: &Path, tool_id: &str, fill: Fill) -> Result<Option<Self>> {
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
        remove_orphans(&git, &repo_root).await;

        let snapshot =
            snapshot_working_tree(&git, &repo_root, "code-assistant: sub-agent base").await?;
        let id: String = tool_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let branch = format!("{BRANCH_PREFIX}{id}-{}", &snapshot[..8]);
        let path = worktree::suggest_worktree_path(&repo_root, &branch);
        let (worktree, base) =
            fill_worktree(&git, &repo_root, &path, &branch, snapshot, fill).await?;
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

    /// End the workspace: commit what the sub-agent left in it to its branch
    /// and remove the worktree. `None` when it changed nothing; the branch
    /// goes away then too. On an error the worktree stays where it is.
    pub async fn close(self) -> Result<Option<KeptChanges>> {
        let changes = snapshot_working_tree(
            &self.git,
            &self.worktree,
            "code-assistant: sub-agent changes",
        )
        .await?;
        let files = changed_paths(&self.git, &self.repo_root, &self.base, &changes).await?;
        if files.is_empty() {
            self.remove(false).await;
            return Ok(None);
        }
        self.git
            .run(
                &self.repo_root,
                &[
                    "update-ref",
                    &format!("refs/heads/{}", self.branch),
                    &changes,
                ],
            )
            .await?;
        self.remove(true).await;
        Ok(Some(KeptChanges {
            repo_root: self.repo_root,
            branch: self.branch,
            base: self.base,
            files,
        }))
    }

    async fn remove(&self, keep_branch: bool) {
        let _ = unlock(&self.git, &self.repo_root, &self.worktree).await;
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

impl KeptChanges {
    /// How the parent reviews and applies the changes, and cleans up.
    pub fn instructions(&self) -> String {
        let root = self.repo_root.display();
        let (base, branch) = (&self.base, &self.branch);
        format!(
            "Its changes are on branch `{branch}`: {files}.\n\
             - Review them: `git -C {root} diff {base} {branch}`\n\
             - Apply them to your working tree: `git -C {root} diff --binary {base} {branch} | git -C {root} apply` \
             (all or nothing; if it fails because you changed these files meanwhile, apply the changes by hand)\n\
             - Then delete the branch: `git -C {root} branch -D {branch}`",
            files = self.files.join(", "),
        )
    }
}

/// Create the worktree at `path` on `branch`, starting from `snapshot` (of
/// `repo_root`), and fill it. Returns the worktree and the commit its files
/// start from.
async fn fill_worktree(
    git: &GitBinary,
    repo_root: &Path,
    path: &Path,
    branch: &str,
    snapshot: String,
    fill: Fill,
) -> Result<(PathBuf, String)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let path_str = path.to_str().context("Worktree path must be valid UTF-8")?;
    let mut args = vec!["--no-optional-locks", "worktree", "add"];
    if fill == Fill::Copy {
        args.push("--no-checkout");
    }
    args.extend(["-b", branch, "--", path_str, &snapshot]);
    git.run(repo_root, &args)
        .await
        .context("Failed to create a worktree for the sub-agent")?;
    let worktree = path.canonicalize()?;
    lock(git, repo_root, &worktree, Some(&snapshot)).await?;
    if fill == Fill::Checkout {
        return Ok((worktree, snapshot));
    }

    // Other worktrees inside the parent's tree are not part of it.
    let skip = nested_worktrees(git, repo_root).await;
    if let Err(e) = clone_tree(repo_root, &worktree, &skip).await {
        tracing::info!("No copy of the project for the sub-agent ({e}); checking it out instead");
        clear_except_git(&worktree)?;
        git.run(&worktree, &["reset", "-q", "--hard"]).await?;
        return Ok((worktree, snapshot));
    }
    // The parent's tree may have changed since its snapshot: start from what
    // was copied, so only the sub-agent's own changes count as its changes.
    let start = snapshot_working_tree(git, &worktree, "code-assistant: sub-agent base").await?;
    git.run(&worktree, &["update-ref", "HEAD", &start]).await?;
    git.run(&worktree, &["read-tree", &start]).await?;
    unlock(git, repo_root, &worktree).await?;
    lock(git, repo_root, &worktree, Some(&start)).await?;
    Ok((worktree, start))
}

/// Lock `worktree` for this process, noting the commit its files start from.
async fn lock(
    git: &GitBinary,
    repo_root: &Path,
    worktree: &Path,
    base: Option<&str>,
) -> Result<()> {
    let mut reason = format!("{LOCK_PREFIX} pid={}", std::process::id());
    if let Some(base) = base {
        reason.push_str(&format!(" base={base}"));
    }
    let path = worktree.to_string_lossy();
    git.run(
        repo_root,
        &["worktree", "lock", "--reason", &reason, "--", &path],
    )
    .await?;
    Ok(())
}

async fn unlock(git: &GitBinary, repo_root: &Path, worktree: &Path) -> Result<()> {
    let path = worktree.to_string_lossy();
    git.run(repo_root, &["worktree", "unlock", "--", &path])
        .await?;
    Ok(())
}

/// A worktree as `git worktree list --porcelain` describes it.
#[derive(Debug, Default, PartialEq, Eq)]
struct ListedWorktree {
    path: PathBuf,
    /// Short branch name, `None` when detached.
    branch: Option<String>,
    /// The lock reason (empty without one), `None` when not locked.
    locked: Option<String>,
}

async fn list_worktrees(git: &GitBinary, repo_root: &Path) -> Vec<ListedWorktree> {
    match git
        .run(repo_root, &["worktree", "list", "--porcelain"])
        .await
    {
        Ok(list) => parse_worktree_list(&list),
        Err(_) => Vec::new(),
    }
}

fn parse_worktree_list(porcelain: &str) -> Vec<ListedWorktree> {
    let mut worktrees: Vec<ListedWorktree> = Vec::new();
    for line in porcelain.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            worktrees.push(ListedWorktree {
                path: PathBuf::from(path),
                ..ListedWorktree::default()
            });
        } else if let Some(current) = worktrees.last_mut() {
            if let Some(branch) = line.strip_prefix("branch ") {
                current.branch = Some(branch.trim_start_matches("refs/heads/").to_string());
            } else if line == "locked" {
                current.locked = Some(String::new());
            } else if let Some(reason) = line.strip_prefix("locked ") {
                current.locked = Some(reason.to_string());
            }
        }
    }
    worktrees
}

/// The worktrees of the repository at `repo_root` that lie inside it.
async fn nested_worktrees(git: &GitBinary, repo_root: &Path) -> Vec<PathBuf> {
    list_worktrees(git, repo_root)
        .await
        .into_iter()
        .filter_map(|w| w.path.canonicalize().ok())
        .filter(|path| path != repo_root && path.starts_with(repo_root))
        .collect()
}

/// Remove the sub-agent worktrees of processes that are gone, keeping what
/// they changed on their branches.
async fn remove_orphans(git: &GitBinary, repo_root: &Path) {
    for listed in list_worktrees(git, repo_root).await {
        let (Some(branch), Some(reason)) = (&listed.branch, &listed.locked) else {
            continue;
        };
        let Some(fields) = reason.strip_prefix(LOCK_PREFIX) else {
            continue;
        };
        if !branch.starts_with(BRANCH_PREFIX) {
            continue;
        }
        let field = |name: &str| {
            fields
                .split_whitespace()
                .find_map(|f| f.strip_prefix(name)?.strip_prefix('='))
                .map(str::to_string)
        };
        let Some(pid) = field("pid").and_then(|pid| pid.parse::<u32>().ok()) else {
            continue;
        };
        if process_alive(pid) {
            continue;
        }
        let orphan = ChildWorkspace {
            git: git.clone(),
            repo_root: repo_root.to_path_buf(),
            worktree: listed.path.clone(),
            branch: branch.clone(),
            base: field("base").unwrap_or_default(),
            project_subdir: PathBuf::new(),
        };
        if orphan.base.is_empty() {
            // It never got filled: nothing to keep.
            orphan.remove(false).await;
            continue;
        }
        match orphan.close().await {
            Ok(Some(kept)) => tracing::info!(
                "Removed the worktree of a sub-agent whose process is gone; its changes are on branch {}",
                kept.branch
            ),
            Ok(None) => {}
            Err(e) => tracing::warn!(
                "Failed to clean up the sub-agent worktree {}: {e:#}",
                listed.path.display()
            ),
        }
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 sends nothing; it only checks that the process exists.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    // EPERM: it exists, but belongs to someone else.
    found || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    // Without a way to tell, never take another process's worktree.
    true
}

/// Remove everything in `dir` but its `.git`: a partial copy.
fn clear_except_git(dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = process_env::std_command("git")
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

    fn sh(dir: &Path, script: &str) {
        let output = process_env::std_command("sh")
            .args(["-c", script])
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "{script}: {output:?}");
    }

    /// The commands in `instructions` between backticks after `label`.
    fn command_after(instructions: &str, label: &str) -> String {
        let line = instructions
            .lines()
            .find(|line| line.contains(label))
            .unwrap();
        line.split('`').nth(1).unwrap().to_string()
    }

    #[tokio::test]
    async fn changes_stay_on_a_branch_and_the_worktree_goes_away() {
        let dir = repo();
        let repo = dir.path().join("repo");
        // Uncommitted parent work is part of the child's starting point.
        std::fs::write(repo.join("sub/draft.txt"), "draft\n").unwrap();

        let ws = ChildWorkspace::create(&repo.join("sub"), "tool_1", Fill::Checkout)
            .await
            .unwrap()
            .unwrap();
        let project = ws.project_dir();
        assert!(project.ends_with("sub"));
        assert_eq!(
            std::fs::read_to_string(project.join("draft.txt")).unwrap(),
            "draft\n"
        );
        std::fs::write(project.join("a.txt"), "ONE\ntwo\nthree\n").unwrap();
        let worktree = ws.root().to_path_buf();

        let kept = ws.close().await.unwrap().expect("changes are kept");

        assert_eq!(kept.files, vec!["sub/a.txt"]);
        assert!(!worktree.exists());
        // Nothing reached the parent's tree.
        assert_eq!(
            std::fs::read_to_string(repo.join("sub/a.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
        assert_eq!(
            git(&repo, &["show", &format!("{}:sub/a.txt", kept.branch)]),
            "ONE\ntwo\nthree"
        );
    }

    #[tokio::test]
    async fn the_reported_commands_apply_the_changes_and_remove_the_branch() {
        let dir = repo();
        let repo = dir.path().join("repo");
        std::fs::write(repo.join("sub/draft.txt"), "draft\n").unwrap();
        let ws = ChildWorkspace::create(&repo, "tool_2", Fill::Checkout)
            .await
            .unwrap()
            .unwrap();
        std::fs::write(ws.project_dir().join("sub/a.txt"), "ONE\ntwo\nthree\n").unwrap();
        std::fs::write(ws.project_dir().join("sub/new.txt"), "new\n").unwrap();
        let kept = ws.close().await.unwrap().unwrap();
        // The parent went on working meanwhile.
        std::fs::write(repo.join("sub/draft.txt"), "draft, edited\n").unwrap();

        let instructions = kept.instructions();
        sh(dir.path(), &command_after(&instructions, "Apply"));
        sh(
            dir.path(),
            &command_after(&instructions, "delete the branch"),
        );

        assert_eq!(
            std::fs::read_to_string(repo.join("sub/a.txt")).unwrap(),
            "ONE\ntwo\nthree\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("sub/new.txt")).unwrap(),
            "new\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("sub/draft.txt")).unwrap(),
            "draft, edited\n"
        );
        assert_eq!(git(&repo, &["diff", "--cached", "--name-only"]), "");
        assert_eq!(branches(&repo), "");
    }

    #[tokio::test]
    async fn no_changes_leave_no_branch() {
        let dir = repo();
        let repo = dir.path().join("repo");
        let ws = ChildWorkspace::create(&repo, "tool_3", Fill::Checkout)
            .await
            .unwrap()
            .unwrap();
        let worktree = ws.root().to_path_buf();

        assert_eq!(ws.close().await.unwrap(), None);
        assert!(!worktree.exists());
        assert_eq!(branches(&repo), "");
    }

    #[tokio::test]
    async fn a_checkout_leaves_ignored_files_out() {
        let dir = repo();
        let repo = dir.path().join("repo");
        std::fs::write(repo.join(".gitignore"), "build/\n").unwrap();
        std::fs::create_dir_all(repo.join("build")).unwrap();
        std::fs::write(repo.join("build/out.bin"), "built").unwrap();

        let ws = ChildWorkspace::create(&repo, "tool_4", Fill::Checkout)
            .await
            .unwrap()
            .unwrap();

        assert!(ws.project_dir().join(".gitignore").exists());
        assert!(!ws.project_dir().join("build").exists());
        ws.close().await.unwrap();
    }

    /// Clones need a file system that supports them; elsewhere a copy falls
    /// back to a checkout.
    #[cfg(target_vendor = "apple")]
    #[tokio::test]
    async fn a_copy_holds_ignored_files_too_and_starts_clean() {
        let dir = repo();
        let repo = dir.path().join("repo");
        std::fs::write(repo.join(".gitignore"), "build/\n").unwrap();
        git(&repo, &["add", ".gitignore"]);
        git(&repo, &["commit", "-q", "-m", "ignore"]);
        std::fs::create_dir_all(repo.join("build")).unwrap();
        std::fs::write(repo.join("build/out.bin"), "built").unwrap();
        std::fs::write(repo.join("sub/draft.txt"), "draft\n").unwrap();
        // A worktree of the repository inside it is not copied.
        git(&repo, &["worktree", "add", "-q", "nested", "-b", "other"]);

        let ws = ChildWorkspace::create(&repo, "tool_5", Fill::Copy)
            .await
            .unwrap()
            .unwrap();
        let copy = ws.project_dir();
        assert_eq!(
            std::fs::read_to_string(copy.join("build/out.bin")).unwrap(),
            "built"
        );
        assert!(!copy.join("nested").exists());
        assert_eq!(git(&copy, &["status", "--porcelain"]), "");

        std::fs::write(copy.join("build/out.bin"), "rebuilt").unwrap();
        std::fs::write(copy.join("sub/a.txt"), "ONE\ntwo\nthree\n").unwrap();
        let kept = ws.close().await.unwrap().unwrap();

        // Ignored files are not part of the changes.
        assert_eq!(kept.files, vec!["sub/a.txt"]);
        assert_eq!(
            std::fs::read_to_string(repo.join("build/out.bin")).unwrap(),
            "built"
        );
    }

    #[tokio::test]
    async fn worktrees_of_a_process_that_is_gone_are_removed_and_their_changes_kept() {
        let dir = repo();
        let repo = dir.path().join("repo");
        let orphan = ChildWorkspace::create(&repo, "tool_6", Fill::Checkout)
            .await
            .unwrap()
            .unwrap();
        std::fs::write(orphan.project_dir().join("sub/a.txt"), "half\n").unwrap();
        let (orphan_root, orphan_branch) = (orphan.root().to_path_buf(), orphan.branch.clone());
        // Its process died: hand the lock to a pid that no longer runs.
        let mut finished = process_env::std_command("true").spawn().unwrap();
        let dead_pid = finished.id();
        finished.wait().unwrap();
        unlock(&orphan.git, &repo, &orphan_root).await.unwrap();
        git(
            &repo,
            &[
                "worktree",
                "lock",
                "--reason",
                &format!("{LOCK_PREFIX} pid={dead_pid} base={}", orphan.base),
                orphan_root.to_str().unwrap(),
            ],
        );
        drop(orphan);

        // The next workspace in the repository cleans up the orphan ...
        let alive = ChildWorkspace::create(&repo, "tool_7", Fill::Checkout)
            .await
            .unwrap()
            .unwrap();
        // ... but not the worktrees of running processes.
        let third = ChildWorkspace::create(&repo, "tool_8", Fill::Checkout)
            .await
            .unwrap()
            .unwrap();

        assert!(!orphan_root.exists());
        assert_eq!(
            git(&repo, &["show", &format!("{orphan_branch}:sub/a.txt")]),
            "half"
        );
        assert!(alive.root().exists());
        alive.close().await.unwrap();
        third.close().await.unwrap();
    }

    #[test]
    fn worktree_list_is_parsed_with_branches_and_lock_reasons() {
        let listed = parse_worktree_list(
            "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n\
             worktree /wt/a\nHEAD def\nbranch refs/heads/code-assistant/sub-agent/x\nlocked code-assistant sub-agent pid=1\n\n\
             worktree /wt/b\nHEAD 123\ndetached\nlocked\n",
        );
        assert_eq!(
            listed,
            vec![
                ListedWorktree {
                    path: "/repo".into(),
                    branch: Some("main".into()),
                    locked: None,
                },
                ListedWorktree {
                    path: "/wt/a".into(),
                    branch: Some("code-assistant/sub-agent/x".into()),
                    locked: Some("code-assistant sub-agent pid=1".into()),
                },
                ListedWorktree {
                    path: "/wt/b".into(),
                    branch: None,
                    locked: Some(String::new()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn no_workspace_outside_git() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            ChildWorkspace::create(dir.path(), "tool_9", Fill::Copy)
                .await
                .unwrap()
                .is_none()
        );
    }
}
