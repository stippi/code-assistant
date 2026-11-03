//! Who can be asked for a review, and which labels can be applied.

use super::detail::RawReviewRequest;
use super::{Connection, trimmed};
use crate::model::{
    LabelCandidate, LabelCandidates, ReviewerCandidate, ReviewerCandidates, ReviewerKind,
};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ReviewerCandidatesData {
    repository: Option<RawReviewerRepository>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawReviewerRepository {
    assignable_users: Connection<RawUser>,
    pull_request: Option<RawReviewerPullRequest>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawUser {
    login: Option<String>,
    name: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawReviewerPullRequest {
    author: Option<RawUser>,
    review_requests: Connection<RawReviewRequest>,
}

/// Assignable users (what GitHub's own picker offers), minus the author,
/// after everyone already asked, so a request can also be taken back.
pub(crate) fn reviewer_candidates(data: ReviewerCandidatesData) -> Option<ReviewerCandidates> {
    let repository = data.repository?;
    let pr = repository.pull_request.unwrap_or_default();
    let author = pr.author.as_ref().and_then(|a| trimmed(a.login.as_deref()));
    let mut candidates: Vec<ReviewerCandidate> = Vec::new();
    for raw in pr
        .review_requests
        .nodes()
        .filter_map(|r| r.requested_reviewer.as_ref())
    {
        let slug = trimmed(raw.slug.as_deref());
        let Some(id) = slug.clone().or_else(|| trimmed(raw.login.as_deref())) else {
            continue;
        };
        let kind = if slug.is_some() {
            ReviewerKind::Team
        } else {
            ReviewerKind::User
        };
        candidates.retain(|c| !(c.kind == kind && c.id == id));
        candidates.push(ReviewerCandidate {
            login: match kind {
                ReviewerKind::Team => trimmed(raw.combined_slug.as_deref()).unwrap_or(id.clone()),
                ReviewerKind::User => id.clone(),
            },
            name: trimmed(raw.team_name.as_deref()).or_else(|| trimmed(raw.name.as_deref())),
            avatar_url: trimmed(raw.team_avatar_url.as_deref())
                .or_else(|| trimmed(raw.avatar_url.as_deref())),
            id,
            kind,
            is_requested: true,
        });
    }
    for user in repository.assignable_users.nodes() {
        let Some(login) = trimmed(user.login.as_deref()) else {
            continue;
        };
        let taken = candidates
            .iter()
            .any(|c| c.kind == ReviewerKind::User && c.id == login);
        if Some(&login) == author.as_ref() || taken {
            continue;
        }
        candidates.push(ReviewerCandidate {
            id: login.clone(),
            kind: ReviewerKind::User,
            login,
            name: trimmed(user.name.as_deref()),
            avatar_url: trimmed(user.avatar_url.as_deref()),
            is_requested: false,
        });
    }
    Some(ReviewerCandidates {
        candidates,
        truncated: repository.assignable_users.has_next_page(),
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct LabelCandidatesData {
    repository: Option<RawLabelRepository>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawLabelRepository {
    labels: Connection<RawRepositoryLabel>,
    pull_request: Option<RawLabelPullRequest>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawRepositoryLabel {
    name: Option<String>,
    color: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawLabelPullRequest {
    labels: Connection<RawRepositoryLabel>,
}

/// The repository's labels; an applied label the repository no longer
/// defines stays listed first, so it can still be taken off.
pub(crate) fn label_candidates(data: LabelCandidatesData) -> Option<LabelCandidates> {
    let repository = data.repository?;
    let applied: Vec<String> = repository
        .pull_request
        .as_ref()
        .map(|pr| {
            pr.labels
                .nodes()
                .filter_map(|l| trimmed(l.name.as_deref()))
                .collect()
        })
        .unwrap_or_default();
    let mut defined: Vec<LabelCandidate> = Vec::new();
    for label in repository.labels.nodes() {
        let Some(name) = trimmed(label.name.as_deref()) else {
            continue;
        };
        if defined.iter().any(|c| c.name == name) {
            continue;
        }
        defined.push(LabelCandidate {
            is_applied: applied.contains(&name),
            name,
            color: trimmed(label.color.as_deref()),
            description: trimmed(label.description.as_deref()),
        });
    }
    let mut candidates: Vec<LabelCandidate> = applied
        .iter()
        .filter(|name| !defined.iter().any(|c| &c.name == *name))
        .map(|name| LabelCandidate {
            name: name.clone(),
            color: None,
            description: None,
            is_applied: true,
        })
        .collect();
    candidates.extend(defined);
    Some(LabelCandidates {
        candidates,
        truncated: repository.labels.has_next_page(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn leaves_the_author_out_and_keeps_requested_teams_apart() {
        let data = serde_json::from_value(json!({"repository": {
            "assignableUsers": {
                "pageInfo": {"hasNextPage": true},
                "nodes": [
                    {"login": "author", "name": null, "avatarUrl": null},
                    {"login": "octocat", "name": "The Octocat", "avatarUrl": "a"},
                    {"login": "core", "name": "A user named like the team", "avatarUrl": null}
                ]
            },
            "pullRequest": {
                "author": {"login": "author"},
                "reviewRequests": {"nodes": [
                    {"requestedReviewer": {"login": "octocat", "name": "The Octocat", "avatarUrl": "a"}},
                    {"requestedReviewer": {"slug": "core", "combinedSlug": "acme/core", "teamName": "Core", "teamAvatarUrl": "t"}},
                    {"requestedReviewer": null}
                ]}
            }
        }}))
        .unwrap();
        let list = reviewer_candidates(data).unwrap();
        let rows: Vec<_> = list
            .candidates
            .iter()
            .map(|c| (c.id.as_str(), c.kind, c.login.as_str(), c.is_requested))
            .collect();
        assert_eq!(
            rows,
            [
                ("octocat", ReviewerKind::User, "octocat", true),
                ("core", ReviewerKind::Team, "acme/core", true),
                ("core", ReviewerKind::User, "core", false),
            ]
        );
        assert!(list.truncated);
    }

    #[test]
    fn keeps_an_applied_label_the_repository_no_longer_defines() {
        let data = serde_json::from_value(json!({"repository": {
            "labels": {
                "pageInfo": {"hasNextPage": false},
                "nodes": [
                    {"name": "bug", "color": "d73a4a", "description": "Something is wrong"},
                    {"name": "docs", "color": "0075ca", "description": null}
                ]
            },
            "pullRequest": {"labels": {"nodes": [{"name": "docs"}, {"name": "legacy"}]}}
        }}))
        .unwrap();
        let list = label_candidates(data).unwrap();
        let rows: Vec<_> = list
            .candidates
            .iter()
            .map(|c| (c.name.as_str(), c.is_applied))
            .collect();
        assert_eq!(rows, [("legacy", true), ("bug", false), ("docs", true)]);
        assert!(!list.truncated);
    }
}
