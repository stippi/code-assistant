//! Snapshot a working tree and merge isolated changes back into it.
//!
//! A writing sub-agent runs in its own worktree, started from a snapshot of
//! its parent's tree (uncommitted and untracked work included). When it
//! finishes, its changes are three-way merged into the parent's working
//! tree. Neither step touches the parent's index, HEAD or branches.

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

/// A path the merge could not resolve on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeConflict {
    pub path: String,
    pub reason: String,
}

/// What [`merge_into_working_tree`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeResult {
    /// Paths written or deleted in the working tree.
    pub applied: Vec<String>,
    /// Paths that changed both here and in the merged changes in ways that
    /// do not combine. When there are any, nothing was applied.
    pub conflicts: Vec<MergeConflict>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TreeEntry {
    mode: String,
    oid: String,
}

impl TreeEntry {
    fn is_plain_file(&self) -> bool {
        self.mode == "100644" || self.mode == "100755"
    }
}

async fn tree_entry(
    git: &GitBinary,
    workdir: &Path,
    commit: &str,
    path: &str,
) -> Result<Option<TreeEntry>> {
    let output = git
        .run(
            workdir,
            &["ls-tree", "-z", "--full-tree", commit, "--", path],
        )
        .await?;
    let Some((meta, _)) = output.split_once('\t') else {
        return Ok(None);
    };
    let mut parts = meta.split(' ');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(mode), Some(_kind), Some(oid)) => Ok(Some(TreeEntry {
            mode: mode.to_string(),
            oid: oid.to_string(),
        })),
        _ => Ok(None),
    }
}

/// The blob id the file at `path` would get if it were added now
/// (clean filters and line-ending conversion applied).
async fn working_oid(git: &GitBinary, workdir: &Path, path: &str) -> Result<Option<String>> {
    let full = workdir.join(path);
    match std::fs::symlink_metadata(&full) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(Some("<not a regular file>".to_string())),
        Err(_) => return Ok(None),
    }
    Ok(Some(
        git.run(workdir, &["hash-object", "--path", path, "--", path])
            .await?,
    ))
}

/// A blob's content as it would be checked out (smudge filters applied).
async fn checkout_bytes(
    git: &GitBinary,
    workdir: &Path,
    commit: &str,
    path: &str,
) -> Result<Vec<u8>> {
    git.run_bytes(
        workdir,
        &["cat-file", "--filters", &format!("{commit}:{path}")],
    )
    .await
}

enum Plan {
    Write { bytes: Vec<u8>, executable: bool },
    Delete,
}

/// Three-way merge the changes from `base` to `changes` into the working
/// tree at `workdir` (its root), leaving the index and HEAD alone.
///
/// Each changed path is compared with what is on disk now: untouched since
/// `base` takes the new version, already equal keeps it, and edits on both
/// sides are combined with `git merge-file`. All or nothing: if any path
/// conflicts, no file is changed and the conflicts are returned.
///
/// The caller must keep other writers off the changed paths while this runs.
pub async fn merge_into_working_tree(
    git: &GitBinary,
    workdir: &Path,
    base: &str,
    changes: &str,
) -> Result<MergeResult> {
    let paths = changed_paths(git, workdir, base, changes).await?;
    let mut plans: Vec<(String, Plan)> = Vec::new();
    let mut conflicts = Vec::new();

    for path in paths {
        let conflict = |reason: &str| MergeConflict {
            path: path.clone(),
            reason: reason.to_string(),
        };
        let base_entry = tree_entry(git, workdir, base, &path).await?;
        let theirs = tree_entry(git, workdir, changes, &path).await?;
        if [&base_entry, &theirs]
            .into_iter()
            .flatten()
            .any(|entry| !entry.is_plain_file())
        {
            conflicts.push(conflict(
                "symlinks and submodules are not merged automatically",
            ));
            continue;
        }
        let ours = working_oid(git, workdir, &path).await?;
        let base_oid = base_entry.as_ref().map(|e| e.oid.clone());
        let theirs_oid = theirs.as_ref().map(|e| e.oid.clone());

        if ours == theirs_oid {
            continue;
        }
        if ours == base_oid {
            let plan = match &theirs {
                Some(entry) => Plan::Write {
                    bytes: checkout_bytes(git, workdir, changes, &path).await?,
                    executable: entry.mode == "100755",
                },
                None => Plan::Delete,
            };
            plans.push((path, plan));
            continue;
        }
        let (Some(_), Some(entry)) = (&base_entry, &theirs) else {
            conflicts.push(conflict(match (&base_entry, &ours, &theirs) {
                (None, _, _) => "added on both sides with different content",
                (_, None, _) => "deleted here but changed by the sub-agent",
                _ => "changed here but deleted by the sub-agent",
            }));
            continue;
        };
        match merge_file(git, workdir, base, changes, &path).await? {
            Some(bytes) => plans.push((
                path,
                Plan::Write {
                    bytes,
                    executable: entry.mode == "100755",
                },
            )),
            None => conflicts.push(conflict("overlapping edits")),
        }
    }

    if !conflicts.is_empty() {
        return Ok(MergeResult {
            applied: Vec::new(),
            conflicts,
        });
    }
    let mut applied = Vec::with_capacity(plans.len());
    for (path, plan) in plans {
        let full = workdir.join(&path);
        match plan {
            Plan::Write { bytes, executable } => write_file(&full, &bytes, executable)?,
            Plan::Delete => match std::fs::remove_file(&full) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(e).with_context(|| format!("Failed to delete {path}"));
                }
                _ => {}
            },
        }
        applied.push(path);
    }
    Ok(MergeResult {
        applied,
        conflicts: Vec::new(),
    })
}

/// Merge both sides' edits of `path`; `None` when they overlap (or the file
/// is binary).
async fn merge_file(
    git: &GitBinary,
    workdir: &Path,
    base: &str,
    changes: &str,
    path: &str,
) -> Result<Option<Vec<u8>>> {
    let scratch = git_path(git, workdir, &format!("merge-{}", unique_suffix())).await?;
    std::fs::create_dir_all(&scratch)?;
    let result = async {
        let base_file = scratch.join("base");
        let theirs_file = scratch.join("theirs");
        std::fs::write(&base_file, checkout_bytes(git, workdir, base, path).await?)?;
        std::fs::write(
            &theirs_file,
            checkout_bytes(git, workdir, changes, path).await?,
        )?;
        let mut cmd = git.command(workdir);
        cmd.arg("merge-file")
            .arg("-p")
            .arg(workdir.join(path))
            .arg(&base_file)
            .arg(&theirs_file);
        let output = cmd.output().await.context("Failed to run git merge-file")?;
        // Exit 0 = clean; a positive count = conflicts; binary input fails.
        Ok(output.status.success().then_some(output.stdout))
    }
    .await;
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn write_file(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        unique_suffix()
    ));
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = match std::fs::metadata(path) {
            Ok(meta) => {
                let mode = meta.permissions().mode();
                if executable {
                    mode | 0o111
                } else {
                    mode & !0o111
                }
            }
            Err(_) if executable => 0o755,
            Err(_) => 0o644,
        };
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::init_repo_with_commit;

    fn git_cmd(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
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
    async fn untouched_files_take_the_new_version() {
        let (dir, git, base) = repo_with_base().await;
        let changes = changes_on(
            &git,
            dir.path(),
            &base,
            &[
                ("a.txt", Some("ONE\ntwo\nthree\nfour\nfive\n")),
                ("b.txt", Some("b\n")),
            ],
        )
        .await;

        let result = merge_into_working_tree(&git, dir.path(), &base, &changes)
            .await
            .unwrap();

        assert!(result.conflicts.is_empty());
        assert_eq!(result.applied, vec!["a.txt", "b.txt"]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "ONE\ntwo\nthree\nfour\nfive\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "b\n"
        );
        assert_eq!(
            git_cmd(dir.path(), &["diff", "--cached", "--name-only"]),
            ""
        );
    }

    #[tokio::test]
    async fn edits_to_different_lines_are_combined() {
        let (dir, git, base) = repo_with_base().await;
        let changes = changes_on(
            &git,
            dir.path(),
            &base,
            &[("a.txt", Some("ONE\ntwo\nthree\nfour\nfive\n"))],
        )
        .await;
        // The parent edited the same file meanwhile, further down.
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\nfour\nFIVE\n").unwrap();

        let result = merge_into_working_tree(&git, dir.path(), &base, &changes)
            .await
            .unwrap();

        assert!(result.conflicts.is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "ONE\ntwo\nthree\nfour\nFIVE\n"
        );
    }

    #[tokio::test]
    async fn overlapping_edits_apply_nothing() {
        let (dir, git, base) = repo_with_base().await;
        let changes = changes_on(
            &git,
            dir.path(),
            &base,
            &[
                ("a.txt", Some("uno\ntwo\nthree\nfour\nfive\n")),
                ("b.txt", Some("b\n")),
            ],
        )
        .await;
        std::fs::write(dir.path().join("a.txt"), "eins\ntwo\nthree\nfour\nfive\n").unwrap();

        let result = merge_into_working_tree(&git, dir.path(), &base, &changes)
            .await
            .unwrap();

        assert_eq!(result.applied, Vec::<String>::new());
        assert_eq!(result.conflicts.len(), 1);
        assert_eq!(result.conflicts[0].path, "a.txt");
        // All or nothing: the clean b.txt was not written either.
        assert!(!dir.path().join("b.txt").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "eins\ntwo\nthree\nfour\nfive\n"
        );
    }

    #[tokio::test]
    async fn deletions_apply_and_conflict_with_edits() {
        let (dir, git, base) = repo_with_base().await;
        let changes = changes_on(&git, dir.path(), &base, &[("a.txt", None)]).await;

        let clean = merge_into_working_tree(&git, dir.path(), &base, &changes)
            .await
            .unwrap();
        assert_eq!(clean.applied, vec!["a.txt"]);
        assert!(!dir.path().join("a.txt").exists());

        std::fs::write(dir.path().join("a.txt"), "changed here\n").unwrap();
        let conflicted = merge_into_working_tree(&git, dir.path(), &base, &changes)
            .await
            .unwrap();
        assert_eq!(conflicted.conflicts.len(), 1);
        assert!(dir.path().join("a.txt").exists());
    }
}
