use crate::binary::GitBinary;
use crate::repository::GitRepository;
use crate::types::Worktree;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// List all worktrees for a repository using gix (no subprocess).
///
/// Always returns the main worktree first. Worktrees with an unborn HEAD
/// (no commits yet) or inaccessible checkout paths are silently skipped.
pub async fn list_worktrees(repo: &GitRepository) -> Result<Vec<Worktree>> {
    let shared = repo.repo.clone();
    tokio::task::spawn_blocking(move || {
        let local = shared.to_thread_local();
        list_worktrees_sync(&local)
    })
    .await
    .context("list_worktrees task panicked")?
}

pub(crate) fn list_worktrees_sync(repo: &gix::Repository) -> Result<Vec<Worktree>> {
    let proxies = repo
        .worktrees()
        .context("Failed to enumerate linked worktrees")?;

    let linked: Result<Vec<_>> = proxies
        .into_iter()
        .filter_map(|proxy| {
            proxy
                .into_repo_with_possibly_inaccessible_worktree()
                .ok()
                .map(|r| build_worktree_entry(&r, false))
        })
        .collect();

    let mut worktrees = Vec::new();
    worktrees.extend(build_worktree_entry(repo, true)?);
    worktrees.extend(linked?.into_iter().flatten());

    Ok(worktrees)
}

fn build_worktree_entry(repo: &gix::Repository, is_main: bool) -> Result<Option<Worktree>> {
    let path = match repo.workdir() {
        Some(p) => p.to_path_buf(),
        None => return Ok(None),
    };

    let head_sha = match repo.head_id() {
        Ok(id) => id.to_hex().to_string(),
        Err(_) => return Ok(None),
    };

    let branch = repo
        .head_name()
        .ok()
        .flatten()
        .map(|name| name.as_bstr().to_string());

    Ok(Some(Worktree {
        path,
        branch,
        head_sha,
        is_main,
    }))
}

/// Create a new worktree with a new branch.
///
/// Creates a new worktree at `path` with a new branch `branch_name`
/// based on `base` (a branch name, tag, or commit SHA). If `base`
/// is `None`, HEAD is used.
///
/// Returns the absolute path to the created worktree.
pub async fn create_worktree(
    git: &GitBinary,
    workdir: &Path,
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

    git.run(
        workdir,
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
    git: &GitBinary,
    workdir: &Path,
    path: &Path,
    branch_name: &str,
) -> Result<PathBuf> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent dir {}", parent.display()))?;
    }

    let path_str = path.to_str().context("Worktree path must be valid UTF-8")?;

    git.run(
        workdir,
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
pub async fn remove_worktree(
    git: &GitBinary,
    workdir: &Path,
    path: &Path,
    force: bool,
) -> Result<()> {
    let path_str = path.to_str().context("Worktree path must be valid UTF-8")?;

    let mut args = vec!["--no-optional-locks", "worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push("--");
    args.push(path_str);

    git.run(workdir, &args).await?;
    Ok(())
}

/// Find the worktree that has the given branch checked out, if any.
pub async fn find_worktree_for_branch(
    repo: &GitRepository,
    branch_name: &str,
) -> Result<Option<Worktree>> {
    let full_ref = format!("refs/heads/{branch_name}");
    Ok(list_worktrees(repo)
        .await?
        .into_iter()
        .find(|wt| wt.branch.as_deref() == Some(&full_ref)))
}

/// Suggest a worktree path for a given branch name.
///
/// Convention: `<parent_of_repo>/.worktrees/<repo_name>/<sanitized_branch>`
pub fn suggest_worktree_path(workdir: &Path, branch_name: &str) -> PathBuf {
    let repo_name = workdir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo");

    let sanitized_branch = branch_name.replace('/', "-");

    let parent = workdir.parent().unwrap_or(workdir);

    parent
        .join(".worktrees")
        .join(repo_name)
        .join(sanitized_branch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GitRepository;
    use crate::testutil::init_repo_with_commit;

    #[test]
    fn test_suggest_worktree_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo_dir = dir.path().join("my-project");
        std::fs::create_dir_all(&repo_dir).unwrap();
        gix::init(&repo_dir).unwrap();

        let repo = GitRepository::open(&repo_dir).unwrap();
        let suggested = suggest_worktree_path(repo.workdir(), "feature/login");

        // Use canonicalize to handle macOS /var -> /private/var symlink
        let expected = dir
            .path()
            .canonicalize()
            .unwrap()
            .join(".worktrees")
            .join("my-project")
            .join("feature-login");

        // Use canonicalize on the root tmpdir to resolve macOS /var -> /private/var symlink,
        // then re-append the relative suffix that suggest_worktree_path would produce
        let root = dir.path().canonicalize().unwrap();
        let suffix = suggested.strip_prefix(dir.path()).unwrap();
        assert_eq!(root.join(suffix), expected);
    }

    // Integration tests that require git binary

    #[tokio::test]
    async fn create_and_list_worktree() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo_dir = dir.path().join("main-repo");
        std::fs::create_dir_all(&repo_dir).unwrap();

        // Need a commit before we can create worktrees
        init_repo_with_commit(&repo_dir);

        let repo = GitRepository::open(&repo_dir).unwrap();

        // Create a worktree
        let wt_path = dir.path().join("worktree-feature");
        create_worktree(&repo.git, repo.workdir(), &wt_path, "feature-branch", None)
            .await
            .unwrap();

        assert!(wt_path.exists());

        // List worktrees
        let worktrees = list_worktrees(&repo).await.unwrap();
        assert_eq!(worktrees.len(), 2);
        assert!(worktrees[0].is_main);
        assert!(!worktrees[1].is_main);
        assert_eq!(worktrees[1].branch_name(), Some("feature-branch"));

        // Find worktree for branch
        let found = find_worktree_for_branch(&repo, "feature-branch")
            .await
            .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().branch_name(), Some("feature-branch"));

        let not_found = find_worktree_for_branch(&repo, "nonexistent")
            .await
            .unwrap();
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn create_and_remove_worktree() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo_dir = dir.path().join("main-repo");
        std::fs::create_dir_all(&repo_dir).unwrap();

        init_repo_with_commit(&repo_dir);

        let repo = GitRepository::open(&repo_dir).unwrap();

        let wt_path = dir.path().join("to-remove");
        create_worktree(&repo.git, repo.workdir(), &wt_path, "temp-branch", None)
            .await
            .unwrap();
        assert!(wt_path.exists());

        remove_worktree(&repo.git, repo.workdir(), &wt_path, false)
            .await
            .unwrap();
        assert!(!wt_path.exists());

        let worktrees = list_worktrees(&repo).await.unwrap();
        assert_eq!(worktrees.len(), 1); // only main remains
    }

    #[tokio::test]
    async fn test_create_worktree_for_existing_branch() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo_dir = dir.path().join("main-repo");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let repo_gix = init_repo_with_commit(&repo_dir);

        // Create an extra branch using gix
        let head_id = repo_gix.head_id().unwrap();
        repo_gix
            .reference(
                "refs/heads/existing-branch",
                head_id,
                gix::refs::transaction::PreviousValue::MustNotExist,
                "branch: Created from HEAD",
            )
            .unwrap();
        drop(repo_gix);

        let repo = GitRepository::open(&repo_dir).unwrap();

        let wt_path = dir.path().join("existing-wt");
        create_worktree_for_branch(&repo.git, repo.workdir(), &wt_path, "existing-branch")
            .await
            .unwrap();
        assert!(wt_path.exists());

        let worktrees = list_worktrees(&repo).await.unwrap();
        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[1].branch_name(), Some("existing-branch"));
    }
}
