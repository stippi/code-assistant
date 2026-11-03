//! Checking out a pull request: its head as git sees it, from what GitHub
//! reports (see [`git::pull_request`] for the checkout itself).

use git::pull_request::{ForkHead, PullRequestHead};
use github::RepoRef;
use github::model::Detail;
use std::path::PathBuf;

/// Where a pull request is checked out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckoutPlace {
    /// In the session's repository: switches the branch it works on.
    Here,
    /// In a worktree of its own, with a session of its own.
    Worktree,
}

/// A checked-out pull request: the session working on it (a new one for a
/// worktree) and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestCheckout {
    pub session_id: String,
    pub path: PathBuf,
    pub branch: String,
    /// At the pull request's head (local commits keep a branch elsewhere).
    pub on_head: bool,
}

/// The head of `detail` for a checkout in `repo`, whose remotes are
/// `(name, url)`: fetched through the remote of the pull request's
/// repository (else `origin`); a fork pushed to over the protocol that
/// remote uses, when its owner lets maintainers push.
pub fn pull_request_head(
    detail: &Detail,
    base: &RepoRef,
    remotes: &[(String, String)],
) -> PullRequestHead {
    let remote = remotes
        .iter()
        .find(|(_, url)| RepoRef::from_remote_url(url).as_ref() == Some(base))
        .or_else(|| remotes.iter().find(|(name, _)| name == "origin"));
    let ssh = remote.is_some_and(|(_, url)| url.starts_with("git@") || url.starts_with("ssh://"));
    let fork = detail.is_cross_repository.then(|| ForkHead {
        owner: detail.head_repository_owner.clone().unwrap_or_default(),
        push_url: detail
            .head_repository
            .as_ref()
            .filter(|_| detail.maintainer_can_modify)
            .map(|name| match ssh {
                true => format!("git@{}:{name}.git", base.host),
                false => format!("https://{}/{name}.git", base.host),
            }),
    });
    PullRequestHead {
        number: detail.number,
        branch: detail.head_branch.clone(),
        sha: detail.head_sha.clone(),
        base_branch: detail.base_branch.clone(),
        remote: remote.map_or_else(|| "origin".to_owned(), |(name, _)| name.clone()),
        fork,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(fork: bool, maintainers: bool) -> Detail {
        let mut detail: Detail = serde_json::from_value(serde_json::json!({
            "number": 8, "title": "t", "body": "", "url": "u", "author": null,
            "state": "open", "is_draft": false, "mergeability": "mergeable",
            "review_decision": null, "additions": 0, "deletions": 0, "changed_files": 0,
            "head_branch": "topic", "head_sha": "abc1234", "head_repository": "contributor/r",
            "head_repository_owner": "contributor", "is_cross_repository": false,
            "base_branch": "main", "created_at": "2026-10-30T10:00:00Z",
            "updated_at": "2026-10-30T10:00:00Z", "merged_at": null, "closed_at": null,
            "reviewers": [], "labels": [], "checks": [], "checks_state": null,
            "checks_truncated": false,
            "merge_capabilities": {"merge": true, "squash": true, "rebase": true},
            "auto_merge_allowed": false,
            "viewer_permissions": {"actions": [], "comment": true, "resolve": true,
                "verdicts": [], "request_reviewers": true, "update_methods": [],
                "labels": true, "maintainer_edits": false},
            "base_comparison": "up-to-date", "behind_by": null,
            "auto_merge_enabled": false, "auto_merge_method": null,
            "maintainer_can_modify": false
        }))
        .unwrap();
        detail.is_cross_repository = fork;
        detail.maintainer_can_modify = maintainers;
        detail
    }

    fn remotes(urls: &[(&str, &str)]) -> Vec<(String, String)> {
        urls.iter()
            .map(|(name, url)| (name.to_string(), url.to_string()))
            .collect()
    }

    #[test]
    fn fetches_through_the_remote_of_the_pull_requests_repository() {
        let base = RepoRef::new("github.com", "stippi", "r");
        let head = pull_request_head(
            &detail(false, false),
            &base,
            &remotes(&[
                ("origin", "git@github.com:me/r.git"),
                ("upstream", "https://github.com/stippi/r.git"),
            ]),
        );
        assert_eq!(head.remote, "upstream");
        assert!(head.fork.is_none());
        assert_eq!((head.branch.as_str(), head.number), ("topic", 8));
    }

    #[test]
    fn a_fork_is_pushed_to_only_when_maintainers_may() {
        let base = RepoRef::new("github.com", "stippi", "r");
        let ssh = remotes(&[("origin", "git@github.com:stippi/r.git")]);
        let head = pull_request_head(&detail(true, true), &base, &ssh);
        let fork = head.fork.unwrap();
        assert_eq!(fork.owner, "contributor");
        assert_eq!(
            fork.push_url.as_deref(),
            Some("git@github.com:contributor/r.git")
        );
        let head = pull_request_head(&detail(true, false), &base, &ssh);
        assert_eq!(head.fork.unwrap().push_url, None);
        let https = remotes(&[("origin", "https://github.com/stippi/r.git")]);
        let head = pull_request_head(&detail(true, true), &base, &https);
        assert_eq!(
            head.fork.unwrap().push_url.as_deref(),
            Some("https://github.com/contributor/r.git")
        );
    }
}
