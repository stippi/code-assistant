//! Snapshots of a working tree as commits.
//!
//! A sub-agent's worktree starts from a snapshot of its parent's tree
//! (uncommitted and untracked work included), and what it changed is a
//! snapshot of the worktree. Taking one touches neither the index, HEAD nor
//! the branches of the tree.

use crate::GitBinary;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Identity for the internal snapshot commits, so they work without a
/// configured `user.name`/`user.email`.
const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "code-assistant"),
    ("GIT_AUTHOR_EMAIL", "code-assistant@localhost"),
    ("GIT_COMMITTER_NAME", "code-assistant"),
    ("GIT_COMMITTER_EMAIL", "code-assistant@localhost"),
];

fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

async fn run_env(
    git: &GitBinary,
    dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<String> {
    let mut cmd = git.command(dir);
    cmd.args(args);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let output = cmd
        .output()
        .await
        .context("Failed to execute git command")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

async fn git_path(git: &GitBinary, workdir: &Path, name: &str) -> Result<PathBuf> {
    let path = PathBuf::from(git.run(workdir, &["rev-parse", "--git-path", name]).await?);
    Ok(if path.is_absolute() {
        path
    } else {
        workdir.join(path)
    })
}

/// Commit the full state of the working tree at `workdir` (its root):
/// tracked changes and untracked files that are not ignored. The commit's
/// parent is HEAD (none in a repository without commits). The real index,
/// HEAD and branches are left alone. Returns the commit id.
pub async fn snapshot_working_tree(
    git: &GitBinary,
    workdir: &Path,
    message: &str,
) -> Result<String> {
    // Stage into a private copy of the index; starting from the real one
    // keeps git's stat cache, so unchanged files are not re-hashed.
    let index = git_path(git, workdir, "index").await?;
    let tmp_index = index.with_file_name(format!("index.snapshot-{}", unique_suffix()));
    if index.exists() {
        std::fs::copy(&index, &tmp_index)
            .with_context(|| format!("Failed to copy {}", index.display()))?;
    }
    let tmp_index_str = tmp_index.to_string_lossy().to_string();
    let index_env = [("GIT_INDEX_FILE", tmp_index_str.as_str())];

    let result = async {
        run_env(git, workdir, &["add", "-A", "--", "."], &index_env).await?;
        let tree = run_env(git, workdir, &["write-tree"], &index_env).await?;
        let head = git
            .run(workdir, &["rev-parse", "--verify", "-q", "HEAD^{commit}"])
            .await
            .ok();
        let mut args = vec!["commit-tree", tree.as_str(), "-m", message];
        if let Some(head) = &head {
            args.extend(["-p", head.as_str()]);
        }
        run_env(git, workdir, &args, &IDENTITY).await
    }
    .await;
    let _ = std::fs::remove_file(&tmp_index);
    result
}

/// Paths (relative to the repository root) that differ between two commits.
pub async fn changed_paths(
    git: &GitBinary,
    workdir: &Path,
    from: &str,
    to: &str,
) -> Result<Vec<String>> {
    let output = git
        .run(
            workdir,
            &[
                "diff-tree",
                "-r",
                "-z",
                "--name-only",
                "--no-renames",
                from,
                to,
            ],
        )
        .await?;
    Ok(output
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::init_repo_with_commit;

    fn git_cmd(dir: &Path, args: &[&str]) -> String {
        let output = process_env::std_command("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn commit_file(dir: &Path, path: &str, content: &str) {
        std::fs::write(dir.join(path), content).unwrap();
        git_cmd(dir, &["add", path]);
        git_cmd(dir, &["commit", "-q", "-m", path]);
    }

    /// A repo with a committed `a.txt`, and a snapshot taken right away.
    async fn repo_with_base() -> (tempfile::TempDir, GitBinary, String) {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with_commit(dir.path());
        commit_file(dir.path(), "a.txt", "one\ntwo\nthree\nfour\nfive\n");
        let git = GitBinary::new().unwrap();
        let base = snapshot_working_tree(&git, dir.path(), "base")
            .await
            .unwrap();
        (dir, git, base)
    }

    /// Commit `files` (None deletes) on top of `base` without touching the
    /// working tree, the way a sub-agent's worktree would.
    async fn changes_on(
        git: &GitBinary,
        dir: &Path,
        base: &str,
        files: &[(&str, Option<&str>)],
    ) -> String {
        let wt = dir.join(format!("wt-{}", unique_suffix()));
        let wt_str = wt.to_str().unwrap().to_string();
        git_cmd(dir, &["worktree", "add", "-q", "--detach", &wt_str, base]);
        for (path, content) in files {
            match content {
                Some(content) => std::fs::write(wt.join(path), content).unwrap(),
                None => std::fs::remove_file(wt.join(path)).unwrap(),
            }
        }
        let commit = snapshot_working_tree(git, &wt, "changes").await.unwrap();
        git_cmd(dir, &["worktree", "remove", "--force", &wt_str]);
        commit
    }

    #[tokio::test]
    async fn snapshot_includes_uncommitted_and_untracked_work_without_staging_it() {
        let (dir, git, _) = repo_with_base().await;
        std::fs::write(dir.path().join("a.txt"), "edited\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "x\n").unwrap();

        let snapshot = snapshot_working_tree(&git, dir.path(), "snap")
            .await
            .unwrap();

        let files = git_cmd(dir.path(), &["ls-tree", "--name-only", &snapshot]);
        assert!(files.contains("new.txt"));
        assert!(!files.contains("ignored.txt"));
        assert_eq!(
            git_cmd(dir.path(), &["show", &format!("{snapshot}:a.txt")]),
            "edited"
        );
        // Nothing was staged and HEAD did not move.
        assert_eq!(
            git_cmd(dir.path(), &["diff", "--cached", "--name-only"]),
            ""
        );
        assert_eq!(
            git_cmd(dir.path(), &["rev-parse", &format!("{snapshot}^")]),
            git_cmd(dir.path(), &["rev-parse", "HEAD"])
        );
    }

    #[tokio::test]
    async fn changed_paths_lists_edits_additions_and_deletions() {
        let (dir, git, _) = repo_with_base().await;
        commit_file(dir.path(), "gone.txt", "x\n");
        let base = snapshot_working_tree(&git, dir.path(), "base")
            .await
            .unwrap();
        let changes = changes_on(
            &git,
            dir.path(),
            &base,
            &[
                ("a.txt", Some("edited\n")),
                ("b.txt", Some("b\n")),
                ("gone.txt", None),
            ],
        )
        .await;

        let paths = changed_paths(&git, dir.path(), &base, &changes)
            .await
            .unwrap();

        assert_eq!(paths, vec!["a.txt", "b.txt", "gone.txt"]);
    }
}
