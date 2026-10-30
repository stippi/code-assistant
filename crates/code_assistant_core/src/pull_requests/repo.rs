//! Which GitHub repository a checkout belongs to, and which pull request
//! is a branch's own.

use github::RepoRef;
use github::model::HeadPullRequest;
use std::path::PathBuf;

/// The GitHub repository of a checkout: its `origin` remote, else the first
/// remote (by name) whose URL names a GitHub repository.
pub(crate) fn github_remote(repo: &git::GitRepository) -> Option<RepoRef> {
    let remotes = repo.remotes();
    let origin = remotes
        .iter()
        .find(|remote| remote.name == "origin")
        .and_then(|remote| RepoRef::from_remote_url(&remote.url));
    origin.or_else(|| {
        remotes
            .iter()
            .find_map(|remote| RepoRef::from_remote_url(&remote.url))
    })
}

/// [`github_remote`] of the checkout at `path`, opened off the async
/// threads.
pub(crate) async fn github_repo(path: PathBuf) -> Option<RepoRef> {
    tokio::task::spawn_blocking(move || github_remote(&git::GitRepository::open(&path).ok()?))
        .await
        .ok()
        .flatten()
}

/// Among the pull requests GitHub found for a branch (newest first), the
/// newest opened from the repository itself, not from a fork that happens
/// to use the same branch name.
pub(crate) fn own_pull_request(found: &[HeadPullRequest]) -> Option<u64> {
    found
        .iter()
        .find(|pr| !pr.is_cross_repository)
        .map(|pr| pr.number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(status.success(), "git {args:?} failed");
    }

    fn repo_with_remotes(dir: &Path, remotes: &[(&str, &str)]) -> git::GitRepository {
        git(dir, &["init", "-q", "-b", "main"]);
        for (name, url) in remotes {
            git(dir, &["remote", "add", name, url]);
        }
        git::GitRepository::open(dir).unwrap()
    }

    #[test]
    fn origin_wins_over_other_github_remotes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with_remotes(
            tmp.path(),
            &[
                ("fork", "git@github.com:me/tool.git"),
                ("origin", "https://github.com/team/tool.git"),
            ],
        );
        assert_eq!(
            github_remote(&repo),
            Some(RepoRef::new("github.com", "team", "tool"))
        );
    }

    #[test]
    fn without_a_github_origin_the_first_github_remote_counts() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with_remotes(
            tmp.path(),
            &[
                ("origin", "/srv/mirrors/tool.git"),
                ("upstream", "git@github.com:team/tool.git"),
                ("zz", "https://github.com/other/tool.git"),
            ],
        );
        assert_eq!(
            github_remote(&repo),
            Some(RepoRef::new("github.com", "team", "tool"))
        );
    }

    #[test]
    fn a_checkout_without_github_remotes_has_no_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with_remotes(tmp.path(), &[("origin", "/srv/mirrors/tool.git")]);
        assert_eq!(github_remote(&repo), None);
    }

    #[test]
    fn a_fork_using_the_same_branch_name_is_not_the_branchs_own() {
        let head = |number, is_cross_repository| HeadPullRequest {
            number,
            title: String::new(),
            url: String::new(),
            head_branch: "feature/x".into(),
            base_branch: "main".into(),
            head_sha: None,
            state: github::model::PullRequestState::Open,
            is_draft: false,
            updated_at: chrono::Utc::now(),
            is_cross_repository,
            head_repository: None,
        };
        assert_eq!(own_pull_request(&[head(9, true), head(7, false)]), Some(7));
        assert_eq!(own_pull_request(&[head(9, true)]), None);
    }
}
