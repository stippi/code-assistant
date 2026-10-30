//! GitHub's JSON → the model. Every raw shape is lenient: a field GitHub
//! leaves out or sends as null takes its default rather than failing the
//! read, and listings decode row by row so one odd row cannot blank a page.

pub(crate) mod activity;
pub(crate) mod candidates;
pub(crate) mod checks;
pub(crate) mod detail;
pub(crate) mod files;
pub(crate) mod list;
pub(crate) mod review;

use crate::error::{Error, Result};
use crate::model::{Actor, Label, MergeMethod, Mergeability, PullRequestState, ReviewDecision};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// A trimmed, non-empty string.
pub(crate) fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Decodes a GraphQL `data` object.
pub(crate) fn decode<T: DeserializeOwned>(operation: &'static str, data: Value) -> Result<T> {
    serde_json::from_value(data).map_err(|e| Error::decode(operation, e))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct PageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

/// A GraphQL connection. Nodes may be null (deleted or unreadable).
#[derive(Debug, Clone, Deserialize)]
#[serde(
    rename_all = "camelCase",
    default,
    bound(deserialize = "T: Deserialize<'de>")
)]
pub(crate) struct Connection<T> {
    pub nodes: Option<Vec<Option<T>>>,
    pub page_info: Option<PageInfo>,
    pub total_count: Option<u64>,
}

impl<T> Default for Connection<T> {
    fn default() -> Self {
        Self {
            nodes: None,
            page_info: None,
            total_count: None,
        }
    }
}

impl<T> Connection<T> {
    pub fn nodes(&self) -> impl Iterator<Item = &T> {
        self.nodes.iter().flatten().flatten()
    }

    pub fn into_nodes(self) -> impl Iterator<Item = T> {
        self.nodes.into_iter().flatten().flatten()
    }

    pub fn has_next_page(&self) -> bool {
        self.page_info.as_ref().is_some_and(|p| p.has_next_page)
    }

    /// GitHub names an end cursor even on the last page; `hasNextPage` decides.
    pub fn next_cursor(&self) -> Option<String> {
        let info = self.page_info.as_ref()?;
        if info.has_next_page {
            trimmed(info.end_cursor.as_deref())
        } else {
            None
        }
    }
}

/// A user, bot, organization or mannequin.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawActor {
    #[serde(rename = "__typename")]
    pub typename: Option<String>,
    pub login: Option<String>,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

pub(crate) fn actor(raw: Option<&RawActor>) -> Option<Actor> {
    let raw = raw?;
    Some(Actor {
        login: trimmed(raw.login.as_deref())?,
        name: trimmed(raw.name.as_deref()),
        avatar_url: trimmed(raw.avatar_url.as_deref()),
        is_bot: raw.typename.as_deref() == Some("Bot"),
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct RawLabel {
    pub name: Option<String>,
    pub color: Option<String>,
}

pub(crate) fn labels(raw: &Connection<RawLabel>) -> Vec<Label> {
    raw.nodes()
        .filter_map(|label| {
            Some(Label {
                name: trimmed(label.name.as_deref())?,
                color: trimmed(label.color.as_deref()),
            })
        })
        .collect()
}

/// A merge time means merged, whatever the state says.
pub(crate) fn state(state: Option<&str>, merged: bool) -> PullRequestState {
    if merged {
        return PullRequestState::Merged;
    }
    match state.map(|s| s.trim().to_ascii_uppercase()).as_deref() {
        Some("MERGED") => PullRequestState::Merged,
        Some("CLOSED") => PullRequestState::Closed,
        _ => PullRequestState::Open,
    }
}

pub(crate) fn mergeability(value: Option<&str>) -> Mergeability {
    match value.map(|s| s.trim().to_ascii_uppercase()).as_deref() {
        Some("MERGEABLE") => Mergeability::Mergeable,
        Some("CONFLICTING") => Mergeability::Conflicting,
        _ => Mergeability::Unknown,
    }
}

pub(crate) fn merge_method(value: Option<&str>) -> Option<MergeMethod> {
    match value.map(|s| s.trim().to_ascii_uppercase()).as_deref() {
        Some("MERGE") => Some(MergeMethod::Merge),
        Some("SQUASH") => Some(MergeMethod::Squash),
        Some("REBASE") => Some(MergeMethod::Rebase),
        _ => None,
    }
}

pub(crate) fn review_decision(value: Option<&str>) -> Option<ReviewDecision> {
    match value.map(|s| s.trim().to_ascii_uppercase()).as_deref() {
        Some("APPROVED") => Some(ReviewDecision::Approved),
        Some("CHANGES_REQUESTED") => Some(ReviewDecision::ChangesRequested),
        Some("REVIEW_REQUIRED") => Some(ReviewDecision::ReviewRequired),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct RawLatestReview {
    pub state: Option<String>,
}

/// GitHub's `reviewDecision` only counts reviews branch rules require, so
/// an app's approval leaves it empty. Fall back to the latest reviews.
pub(crate) fn review_decision_with_reviews(
    value: Option<&str>,
    reviews: &Connection<RawLatestReview>,
) -> Option<ReviewDecision> {
    let summarized = review_decision(value);
    if matches!(
        summarized,
        Some(ReviewDecision::Approved | ReviewDecision::ChangesRequested)
    ) {
        return summarized;
    }
    let has = |wanted: &str| {
        reviews.nodes().any(|review| {
            review
                .state
                .as_deref()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case(wanted))
        })
    };
    if has("CHANGES_REQUESTED") {
        Some(ReviewDecision::ChangesRequested)
    } else if has("APPROVED") {
        Some(ReviewDecision::Approved)
    } else {
        summarized
    }
}

/// The avatar GitHub serves for a login, for actors read without one.
/// Apps have no profile page, so their guessed URLs would 404.
pub(crate) fn login_avatar_url(login: &str, host: &str) -> Option<String> {
    let plausible = !login.is_empty()
        && login.len() <= 39
        && login
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    plausible.then(|| format!("https://{host}/{login}.png?size=80"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_merge_time_means_merged_even_when_the_state_says_closed() {
        assert_eq!(state(Some("CLOSED"), true), PullRequestState::Merged);
        assert_eq!(state(Some("closed"), false), PullRequestState::Closed);
        assert_eq!(state(None, false), PullRequestState::Open);
    }

    #[test]
    fn takes_the_verdict_from_latest_reviews_when_github_summarizes_none() {
        let reviews: Connection<RawLatestReview> = serde_json::from_value(
            json!({"nodes": [{"state": "COMMENTED"}, {"state": "APPROVED"}]}),
        )
        .unwrap();
        assert_eq!(
            review_decision_with_reviews(None, &reviews),
            Some(ReviewDecision::Approved)
        );
        assert_eq!(
            review_decision_with_reviews(Some("REVIEW_REQUIRED"), &reviews),
            Some(ReviewDecision::Approved)
        );
        assert_eq!(
            review_decision_with_reviews(Some("CHANGES_REQUESTED"), &reviews),
            Some(ReviewDecision::ChangesRequested)
        );
    }

    #[test]
    fn the_next_cursor_follows_has_next_page() {
        let last: Connection<Value> = serde_json::from_value(
            json!({"nodes": [], "pageInfo": {"hasNextPage": false, "endCursor": "abc"}}),
        )
        .unwrap();
        assert_eq!(last.next_cursor(), None);
        let more: Connection<Value> = serde_json::from_value(
            json!({"nodes": [null, 1], "pageInfo": {"hasNextPage": true, "endCursor": "abc"}}),
        )
        .unwrap();
        assert_eq!(more.next_cursor().as_deref(), Some("abc"));
        assert_eq!(more.nodes().count(), 1);
    }
}
