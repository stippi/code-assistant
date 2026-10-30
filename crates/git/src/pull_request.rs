//! Checking out a pull request's head the way `gh pr checkout` does, here
//! or in a worktree of its own.
//!
//! - **Branch**: named like the head branch; a fork's head named like the
//!   base repository's default (or base) branch becomes `owner/branch`.
//! - **Fetch**: a branch of the repository itself from its remote
//!   (`+refs/heads/b:refs/remotes/origin/b`); a fork's head as
//!   `refs/pull/N/head` from the base repository, no remote added.
//! - **Upstream**: the remote's branch; for a fork, its URL and branch
//!   when maintainers may push to it, else the base's `refs/pull/N/head`.
//! - **Updating**: an existing branch only fast-forwards; local commits
//!   stay (then the checkout is not on the pull request's head).
//!
//! Here, tracked changes refuse the switch (as in T3 Code). In a worktree,
//! a branch already checked out in another worktree is reused there; one
//! checked out in the repository itself is refused.

use crate::GitRepository;
use crate::worktree::{list_worktrees, suggest_worktree_path};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The head of a pull request, as GitHub reports it.
#[derive(Clone, Debug)]
pub struct PullRequestHead {
    pub number: u64,
    /// The branch in the head repository.
    pub branch: String,
    pub sha: Option<String>,
    /// The pull request's base branch.
    pub base_branch: String,
    /// The base repository's remote here (e.g. `origin`).
    pub remote: String,
    /// Set when the head lives in a fork.
    pub fork: Option<ForkHead>,
}

#[derive(Clone, Debug)]
pub struct ForkHead {
    pub owner: String,
    /// The fork's clone URL, when its owner lets maintainers push.
    pub push_url: Option<String>,
}

/// Where the pull request was checked out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedOut {
    pub path: PathBuf,
    pub branch: String,
    /// The checkout is at the pull request's head (local commits or a
    /// diverged branch keep it elsewhere).
    pub on_head: bool,
}

/// The local branch for `head`, as `gh pr checkout` names it.
pub fn local_branch(head: &PullRequestHead, default_branch: Option<&str>) -> String {
    match &head.fork {
        Some(fork)
            if head.branch == head.base_branch || Some(head.branch.as_str()) == default_branch =>
        {
            format!("{}/{}", fork.owner, head.branch)
        }
        _ => head.branch.clone(),
    }
}

/// What to bring the branch to, and what it tracks.
struct Fetched {
    target: String,
    upstream: Option<(String, String)>,
}

async fn git(repo: &GitRepository, dir: &Path, args: &[&str]) -> Result<String> {
    repo.git.run(dir, args).await
}

async fn has_branch(repo: &GitRepository, branch: &str) -> bool {
    let full = format!("refs/heads/{branch}");
    git(
        repo,
        repo.workdir(),
        &["rev-parse", "--verify", "--quiet", &full],
    )
    .await
    .is_ok()
}

/// The remote's default branch (`origin/HEAD`), when known.
async fn default_branch(repo: &GitRepository, remote: &str) -> Option<String> {
    let head = format!("refs/remotes/{remote}/HEAD");
    let name = git(
        repo,
        repo.workdir(),
        &["symbolic-ref", "--quiet", "--short", &head],
    )
    .await
    .ok()?;
    name.strip_prefix(&format!("{remote}/")).map(str::to_owned)
}

/// Fetch the head: a branch of the repository itself as its remote
/// branch (the pull request's ref when that fails), a fork's head as the
/// pull request's ref.
async fn fetch(repo: &GitRepository, head: &PullRequestHead) -> Result<Fetched> {
    let dir = repo.workdir();
    let pull_ref = format!("refs/pull/{}/head", head.number);
    if head.fork.is_none() {
        let refspec = format!(
            "+refs/heads/{0}:refs/remotes/{1}/{0}",
            head.branch, head.remote
        );
        let fetched = git(
            repo,
            dir,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--",
                &head.remote,
                &refspec,
            ],
        )
        .await;
        if fetched.is_ok() {
            return Ok(Fetched {
                target: format!("refs/remotes/{}/{}", head.remote, head.branch),
                upstream: Some((head.remote.clone(), format!("refs/heads/{}", head.branch))),
            });
        }
    }
    git(
        repo,
        dir,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--",
            &head.remote,
            &pull_ref,
        ],
    )
    .await
    .with_context(|| format!("Could not fetch pull request #{}", head.number))?;
    let target = git(repo, dir, &["rev-parse", "FETCH_HEAD"]).await?;
    let upstream = head.fork.as_ref().map(|fork| match &fork.push_url {
        Some(url) => (url.clone(), format!("refs/heads/{}", head.branch)),
        None => (head.remote.clone(), pull_ref.clone()),
    });
    Ok(Fetched { target, upstream })
}

/// Track the upstream like `gh` does: by remote name or URL.
async fn set_upstream(repo: &GitRepository, branch: &str, fetched: &Fetched) -> Result<()> {
    let Some((remote, merge)) = &fetched.upstream else {
        return Ok(());
    };
    let dir = repo.workdir();
    git(
        repo,
        dir,
        &["config", &format!("branch.{branch}.remote"), remote],
    )
    .await?;
    git(
        repo,
        dir,
        &["config", &format!("branch.{branch}.merge"), merge],
    )
    .await?;
    Ok(())
}

async fn at(repo: &GitRepository, dir: &Path, sha: Option<&str>) -> bool {
    let Some(sha) = sha else {
        return false;
    };
    git(repo, dir, &["rev-parse", "HEAD"])
        .await
        .is_ok_and(|head| head.eq_ignore_ascii_case(sha))
}

async fn is_ancestor(repo: &GitRepository, ancestor: &str, of: &str) -> bool {
    git(
        repo,
        repo.workdir(),
        &["merge-base", "--is-ancestor", ancestor, of],
    )
    .await
    .is_ok()
}

/// Check the pull request out in the repository itself. Tracked changes
/// refuse it.
pub async fn checkout_here(repo: &GitRepository, head: &PullRequestHead) -> Result<CheckedOut> {
    let dir = repo.workdir();
    let changes = git(
        repo,
        dir,
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .await?;
    if !changes.trim().is_empty() {
        bail!(
            "This repository has uncommitted changes. Commit or stash them before checking out \
             pull request #{} here, or check it out in its own worktree.",
            head.number
        );
    }
    let default = default_branch(repo, &head.remote).await;
    let branch = local_branch(head, default.as_deref());
    let fetched = fetch(repo, head).await?;
    if has_branch(repo, &branch).await {
        git(repo, dir, &["checkout", "--quiet", &branch, "--"]).await?;
        git(
            repo,
            dir,
            &["merge", "--ff-only", "--quiet", &fetched.target],
        )
        .await
        .with_context(|| {
            format!("The branch {branch} has commits the pull request does not have")
        })?;
    } else {
        git(
            repo,
            dir,
            &["checkout", "--quiet", "-b", &branch, &fetched.target, "--"],
        )
        .await?;
    }
    set_upstream(repo, &branch, &fetched).await?;
    Ok(CheckedOut {
        path: dir.to_path_buf(),
        on_head: at(repo, dir, head.sha.as_deref()).await,
        branch,
    })
}

/// Check the pull request out in a worktree of its own: the one that has
/// its branch already, else a new one beside the repository.
pub async fn checkout_in_worktree(
    repo: &GitRepository,
    head: &PullRequestHead,
) -> Result<CheckedOut> {
    let default = default_branch(repo, &head.remote).await;
    let branch = local_branch(head, default.as_deref());
    let full = format!("refs/heads/{branch}");
    let holder = list_worktrees(repo)
        .await?
        .into_iter()
        .find(|worktree| worktree.branch.as_deref() == Some(full.as_str()));
    if holder.as_ref().is_some_and(|worktree| worktree.is_main) {
        bail!(
            "The branch {branch} is checked out in the repository itself. Check the pull \
             request out there, or switch the repository to another branch first."
        );
    }
    let fetched = fetch(repo, head).await?;
    let path = match holder {
        Some(worktree) => {
            // Moved on only when nothing there would be lost.
            let clean = git(repo, &worktree.path, &["status", "--porcelain"])
                .await
                .is_ok_and(|status| status.trim().is_empty());
            if clean && is_ancestor(repo, &full, &fetched.target).await {
                git(
                    repo,
                    &worktree.path,
                    &["merge", "--ff-only", "--quiet", &fetched.target],
                )
                .await?;
            }
            worktree.path
        }
        None => {
            if !has_branch(repo, &branch).await {
                git(
                    repo,
                    repo.workdir(),
                    &["branch", "--", &branch, &fetched.target],
                )
                .await?;
            } else if is_ancestor(repo, &full, &fetched.target).await {
                git(
                    repo,
                    repo.workdir(),
                    &["branch", "--force", "--", &branch, &fetched.target],
                )
                .await?;
            }
            let path = free_path(suggest_worktree_path(repo.workdir(), &branch));
            crate::worktree::create_worktree_for_branch(&repo.git, repo.workdir(), &path, &branch)
                .await
                .context("Failed to create the worktree")?
        }
    };
    set_upstream(repo, &branch, &fetched).await?;
    Ok(CheckedOut {
        on_head: at(repo, &path, head.sha.as_deref()).await,
        path,
        branch,
    })
}

/// `path`, or the first of `path-2`, `path-3`, … that does not exist.
fn free_path(path: PathBuf) -> PathBuf {
    let mut candidate = path.clone();
    let mut n = 2;
    while candidate.exists() {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(format!("-{n}"));
        candidate = path.with_file_name(name);
        n += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn run(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=T",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn commit(dir: &Path, file: &str) -> String {
        std::fs::write(dir.join(file), file).unwrap();
        run(dir, &["add", file]);
        run(dir, &["commit", "-q", "-m", file]);
        run(dir, &["rev-parse", "HEAD"])
    }

    /// A "GitHub" repository with `main`, a branch `feature` (#7) and a
    /// fork's head at `refs/pull/8/head`, and a clone of it.
    struct Setup {
        _dir: tempfile::TempDir,
        clone: PathBuf,
        feature: String,
        fork: String,
    }

    fn setup() -> Setup {
        let dir = tempfile::TempDir::new().unwrap();
        let upstream = dir.path().join("upstream");
        std::fs::create_dir(&upstream).unwrap();
        run(&upstream, &["init", "-q", "-b", "main"]);
        commit(&upstream, "a");
        run(&upstream, &["checkout", "-q", "-b", "feature"]);
        let feature = commit(&upstream, "b");
        run(&upstream, &["checkout", "-q", "main"]);
        run(&upstream, &["checkout", "-q", "-b", "fork-work"]);
        let fork = commit(&upstream, "c");
        run(&upstream, &["update-ref", "refs/pull/8/head", &fork]);
        run(&upstream, &["checkout", "-q", "main"]);
        run(&upstream, &["branch", "-q", "-D", "fork-work"]);
        let clone = dir.path().join("clone");
        run(
            dir.path(),
            &[
                "clone",
                "-q",
                upstream.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        Setup {
            _dir: dir,
            clone,
            feature,
            fork,
        }
    }

    fn own_head(sha: &str) -> PullRequestHead {
        PullRequestHead {
            number: 7,
            branch: "feature".into(),
            sha: Some(sha.into()),
            base_branch: "main".into(),
            remote: "origin".into(),
            fork: None,
        }
    }

    fn fork_head(sha: &str, branch: &str, push_url: Option<&str>) -> PullRequestHead {
        PullRequestHead {
            number: 8,
            branch: branch.into(),
            sha: Some(sha.into()),
            base_branch: "main".into(),
            remote: "origin".into(),
            fork: Some(ForkHead {
                owner: "contributor".into(),
                push_url: push_url.map(str::to_owned),
            }),
        }
    }

    #[tokio::test]
    async fn checks_out_a_branch_of_the_repository_here_tracking_its_remote() {
        let setup = setup();
        let repo = GitRepository::open(&setup.clone).unwrap();
        let done = checkout_here(&repo, &own_head(&setup.feature))
            .await
            .unwrap();
        assert_eq!(done.branch, "feature");
        assert!(done.on_head);
        assert_eq!(
            run(&setup.clone, &["rev-parse", "--abbrev-ref", "@{u}"]),
            "origin/feature"
        );
    }

    #[tokio::test]
    async fn checks_out_a_forks_head_from_the_pull_request_ref() {
        let setup = setup();
        let repo = GitRepository::open(&setup.clone).unwrap();
        // The fork's branch is named like the base's default branch.
        let head = fork_head(
            &setup.fork,
            "main",
            Some("https://github.com/contributor/r.git"),
        );
        let done = checkout_here(&repo, &head).await.unwrap();
        assert_eq!(done.branch, "contributor/main");
        assert!(done.on_head);
        let config = |key: &str| run(&setup.clone, &["config", key]);
        assert_eq!(
            config("branch.contributor/main.remote"),
            "https://github.com/contributor/r.git"
        );
        assert_eq!(config("branch.contributor/main.merge"), "refs/heads/main");
        // Without maintainer edits it tracks the pull request's ref.
        let head = fork_head(&setup.fork, "topic", None);
        let done = checkout_here(&repo, &head).await.unwrap();
        assert_eq!(done.branch, "topic");
        assert_eq!(config("branch.topic.merge"), "refs/pull/8/head");
    }

    #[tokio::test]
    async fn tracked_changes_refuse_a_checkout_here() {
        let setup = setup();
        std::fs::write(setup.clone.join("a"), "changed").unwrap();
        let repo = GitRepository::open(&setup.clone).unwrap();
        let refused = checkout_here(&repo, &own_head(&setup.feature))
            .await
            .unwrap_err();
        assert!(
            refused.to_string().contains("uncommitted changes"),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn a_worktree_is_made_once_and_reused() {
        let setup = setup();
        let repo = GitRepository::open(&setup.clone).unwrap();
        let first = checkout_in_worktree(&repo, &own_head(&setup.feature))
            .await
            .unwrap();
        assert!(first.on_head);
        assert_ne!(first.path, setup.clone);
        assert_eq!(run(&first.path, &["branch", "--show-current"]), "feature");
        let again = checkout_in_worktree(&repo, &own_head(&setup.feature))
            .await
            .unwrap();
        assert_eq!(again.path, first.path);
        // The repository itself keeps its branch.
        assert_eq!(run(&setup.clone, &["branch", "--show-current"]), "main");
    }

    #[tokio::test]
    async fn a_branch_checked_out_in_the_repository_is_not_taken_into_a_worktree() {
        let setup = setup();
        let repo = GitRepository::open(&setup.clone).unwrap();
        checkout_here(&repo, &own_head(&setup.feature))
            .await
            .unwrap();
        let refused = checkout_in_worktree(&repo, &own_head(&setup.feature))
            .await
            .unwrap_err();
        assert!(
            refused.to_string().contains("repository itself"),
            "{refused}"
        );
    }
}
