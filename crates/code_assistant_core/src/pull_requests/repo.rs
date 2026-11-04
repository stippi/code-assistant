//! Which GitHub repository a checkout belongs to, and which pull request
//! is a branch's own.

use github::RepoRef;
use github::model::HeadPullRequest;
use std::path::{Path, PathBuf};

use super::RepoCheckout;

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

/// The branch being worked on in a checkout: the one checked out, unless it
/// is the base branch.
pub(crate) fn working_branch(repo: &git::GitRepository) -> Option<String> {
    repo.current_branch().filter(|branch| {
        !repo
            .default_base_branch()
            .is_some_and(|base| crate::session::lifecycle::is_base_branch(branch, &base))
    })
}

/// The GitHub repositories at `dir`: its own, then those of the checkouts
/// directly below it, by directory name. Hidden directories are not looked
/// into, so worktrees under `.worktrees` do not count, and a repository
/// checked out twice is listed once.
pub(crate) fn github_checkouts(dir: &Path) -> Vec<RepoCheckout> {
    let mut below: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let hidden = path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'));
            !hidden && path.join(".git").exists()
        })
        .collect();
    below.sort_by_key(|path| path.to_string_lossy().to_lowercase());
    let mut checkouts: Vec<RepoCheckout> = Vec::new();
    for path in std::iter::once(dir.to_path_buf()).chain(below) {
        let Ok(opened) = git::GitRepository::open(&path) else {
            continue;
        };
        let Some(repo) = github_remote(&opened) else {
            continue;
        };
        if checkouts.iter().all(|checkout| checkout.repo != repo) {
            checkouts.push(RepoCheckout {
                branch: working_branch(&opened),
                path,
                repo,
            });
        }
    }
    checkouts
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
        let status = process_env::std_command("git")
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

    /// A checkout of `github.com/team/<name>` below `dir`.
    fn checkout_below(dir: &Path, directory: &str, name: &str) -> std::path::PathBuf {
        let path = dir.join(directory);
        std::fs::create_dir_all(&path).unwrap();
        let url = format!("git@github.com:team/{name}.git");
        repo_with_remotes(&path, &[("origin", url.as_str())]);
        path
    }

    fn names(checkouts: &[RepoCheckout]) -> Vec<&str> {
        checkouts.iter().map(|c| c.repo.name.as_str()).collect()
    }

    #[test]
    fn a_folder_of_checkouts_lists_each_by_directory_name() {
        let tmp = tempfile::tempdir().unwrap();
        let beta = checkout_below(tmp.path(), "Beta", "beta");
        checkout_below(tmp.path(), "alpha", "alpha");
        // Neither a checkout nor one of GitHub's.
        std::fs::create_dir(tmp.path().join("notes")).unwrap();
        let mirror = tmp.path().join("mirror");
        std::fs::create_dir(&mirror).unwrap();
        repo_with_remotes(&mirror, &[("origin", "/srv/mirrors/tool.git")]);

        let found = github_checkouts(tmp.path());
        assert_eq!(names(&found), ["alpha", "beta"]);
        assert_eq!(found[1].path, beta);
    }

    #[test]
    fn the_folders_own_repository_comes_first() {
        let tmp = tempfile::tempdir().unwrap();
        repo_with_remotes(tmp.path(), &[("origin", "git@github.com:team/root.git")]);
        checkout_below(tmp.path(), "nested", "nested");

        let found = github_checkouts(tmp.path());
        assert_eq!(names(&found), ["root", "nested"]);
        assert_eq!(found[0].path, tmp.path());
    }

    #[test]
    fn hidden_directories_and_second_checkouts_do_not_count() {
        let tmp = tempfile::tempdir().unwrap();
        checkout_below(tmp.path(), "first", "tool");
        checkout_below(tmp.path(), "second", "tool");
        checkout_below(tmp.path(), ".worktrees", "hidden");

        assert_eq!(names(&github_checkouts(tmp.path())), ["tool"]);
    }

    #[test]
    fn a_checkout_on_its_base_branch_has_no_working_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let path = checkout_below(tmp.path(), "tool", "tool");
        git(&path, &["config", "user.email", "t@t.t"]);
        git(&path, &["config", "user.name", "t"]);
        git(&path, &["config", "commit.gpgsign", "false"]);
        git(&path, &["commit", "-q", "--allow-empty", "-m", "init"]);
        assert_eq!(github_checkouts(tmp.path())[0].branch, None);

        git(&path, &["checkout", "-q", "-b", "feature/x"]);
        assert_eq!(
            github_checkouts(tmp.path())[0].branch.as_deref(),
            Some("feature/x")
        );
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
