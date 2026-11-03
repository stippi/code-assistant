//! The conversation: `PullRequestActivity` (remarks, commits),
//! `ReviewThreads` (threads, reactions, reviewers, edit times),
//! `ReviewDismissals` and `ReviewThreadComments`, and how they combine.

use super::detail::{RawReviewRequest, RawReviewer, with_fallback_avatar};
use super::list::RawCommitNode;
use super::{Connection, RawActor, actor, login_avatar_url, trimmed};
use crate::model::{
    Activity, Actor, Comment, CommentKind, Commit, DiffSide, Reaction, ReactionContent,
    ReviewState, ReviewThread, ThreadComment, ThreadCommentsPage,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawLogin {
    login: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawReactors {
    total_count: u64,
    nodes: Option<Vec<Option<RawLogin>>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawReactionGroup {
    content: Option<String>,
    viewer_has_reacted: bool,
    reactors: RawReactors,
}

/// The viewer counts in `count` but is left out of `actors`, so a hover
/// does not list "you" twice.
fn reactions(groups: &[RawReactionGroup], viewer: Option<&str>) -> Vec<Reaction> {
    groups
        .iter()
        .filter_map(|group| {
            let content = ReactionContent::from_graphql(group.content.as_deref()?)?;
            let logins: Vec<String> = group
                .reactors
                .nodes
                .iter()
                .flatten()
                .flatten()
                .filter_map(|node| trimmed(node.login.as_deref()))
                .collect();
            let count = group.reactors.total_count.max(logins.len() as u64);
            if count == 0 {
                return None;
            }
            let actors = logins
                .into_iter()
                .filter(|login| viewer.is_none_or(|viewer| !login.eq_ignore_ascii_case(viewer)))
                .collect();
            Some(Reaction {
                content,
                count: count as u32,
                actors,
                viewer_has_reacted: group.viewer_has_reacted,
            })
        })
        .collect()
}

// --- PullRequestActivity ---------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ActivityData {
    repository: Option<RawActivityRepository>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawActivityRepository {
    pull_request: Option<RawActivityPullRequest>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawActivityPullRequest {
    author: Option<RawActor>,
    commits: Connection<RawCommitNode<RawActivityCommit>>,
    comments: Connection<RawIssueComment>,
    reviews: Connection<RawReview>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawCount {
    total_count: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawCommitUser {
    login: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawCommitAuthor {
    name: Option<String>,
    email: Option<String>,
    avatar_url: Option<String>,
    user: Option<RawCommitUser>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawActivityCommit {
    id: Option<String>,
    oid: Option<String>,
    message_headline: String,
    committed_date: Option<DateTime<Utc>>,
    parents: RawCount,
    authors: Connection<RawCommitAuthor>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawIssueComment {
    id: String,
    #[serde(default)]
    body: Option<String>,
    created_at: DateTime<Utc>,
    #[serde(default)]
    last_edited_at: Option<DateTime<Utc>>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    author: Option<RawActor>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReview {
    id: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    submitted_at: Option<DateTime<Utc>>,
    #[serde(default)]
    last_edited_at: Option<DateTime<Utc>>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    author: Option<RawActor>,
}

/// One page of the activity read.
pub(crate) struct ActivityPage {
    /// Node ids of commits with one parent, whose stats are worth reading.
    pub non_merge_commit_ids: Vec<String>,
    pub author: Option<Actor>,
    pub commits: Vec<Commit>,
    /// Issue comments and reviews, oldest first.
    pub remarks: Vec<Comment>,
    pub next_comments_cursor: Option<String>,
    pub next_reviews_cursor: Option<String>,
}

/// An unlinked commit signature has no login; its name or e-mail stands in.
fn commit_actor(raw: &RawCommitAuthor) -> Option<Actor> {
    let login = raw
        .user
        .as_ref()
        .and_then(|u| trimmed(u.login.as_deref()))
        .or_else(|| trimmed(raw.name.as_deref()))
        .or_else(|| trimmed(raw.email.as_deref()))?;
    Some(Actor {
        login,
        name: trimmed(raw.name.as_deref()),
        avatar_url: trimmed(raw.avatar_url.as_deref()),
        is_bot: false,
    })
}

pub(crate) fn activity_page(data: ActivityData) -> Option<ActivityPage> {
    let pr = data.repository?.pull_request?;
    let commits: Vec<&RawActivityCommit> = pr.commits.nodes().map(|n| &n.commit).collect();
    let issue_comments = pr.comments.nodes().map(|comment| Comment {
        id: comment.id.clone(),
        kind: CommentKind::IssueComment,
        author: actor(comment.author.as_ref()),
        body: comment.body.clone().unwrap_or_default(),
        created_at: comment.created_at,
        edited_at: comment.last_edited_at,
        url: trimmed(comment.url.as_deref()),
        path: None,
        review_state: None,
        reactions: Vec::new(),
    });
    // A bodiless COMMENTED review only holds line comments, read with the threads.
    let reviews = pr.reviews.nodes().filter_map(|review| {
        let submitted_at = review.submitted_at?;
        let state = review.state.as_deref().and_then(ReviewState::from_graphql);
        let body = review.body.clone().unwrap_or_default();
        if body.trim().is_empty() && !state.is_some_and(ReviewState::is_verdict) {
            return None;
        }
        Some(Comment {
            id: review.id.clone(),
            kind: CommentKind::Review,
            author: actor(review.author.as_ref()),
            body,
            created_at: submitted_at,
            edited_at: review.last_edited_at,
            url: trimmed(review.url.as_deref()),
            path: None,
            review_state: state,
            reactions: Vec::new(),
        })
    });
    let mut remarks: Vec<Comment> = issue_comments.chain(reviews).collect();
    remarks.sort_by_key(|c| c.created_at);
    Some(ActivityPage {
        non_merge_commit_ids: commits
            .iter()
            .filter(|c| c.parents.total_count <= 1)
            .filter_map(|c| trimmed(c.id.as_deref()))
            .collect(),
        author: actor(pr.author.as_ref()),
        commits: commits
            .iter()
            .filter_map(|c| {
                Some(Commit {
                    oid: trimmed(c.oid.as_deref())?,
                    message_headline: c.message_headline.clone(),
                    committed_date: c.committed_date?,
                    additions: None,
                    deletions: None,
                    authors: c.authors.nodes().filter_map(commit_actor).collect(),
                })
            })
            .collect(),
        next_comments_cursor: pr.comments.next_cursor(),
        next_reviews_cursor: pr.reviews.next_cursor(),
        remarks,
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct CommitStatsData {
    nodes: Vec<Option<RawCommitStats>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawCommitStats {
    #[serde(rename = "__typename")]
    typename: Option<String>,
    oid: Option<String>,
    additions: i64,
    deletions: i64,
}

/// Additions and deletions per commit oid.
pub(crate) fn commit_stats(data: CommitStatsData) -> HashMap<String, (u64, u64)> {
    data.nodes
        .into_iter()
        .flatten()
        .filter(|c| c.typename.as_deref() == Some("Commit"))
        .filter_map(|c| {
            Some((
                trimmed(c.oid.as_deref())?,
                (c.additions.max(0) as u64, c.deletions.max(0) as u64),
            ))
        })
        .collect()
}

// --- ReviewThreads -----------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ThreadsData {
    viewer: Option<RawLogin>,
    repository: Option<RawThreadsRepository>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawThreadsRepository {
    pull_request: Option<RawThreadsPullRequest>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawThreadsPullRequest {
    review_threads: Connection<RawThread>,
    viewer_can_update: Option<bool>,
    viewer_did_author: Option<bool>,
    author: Option<RawActor>,
    reaction_groups: Vec<RawReactionGroup>,
    comments: Connection<RawReactable>,
    reviews: Connection<RawReactable>,
    review_requests: Connection<RawReviewRequest>,
    latest_reviews: Connection<RawLatestReviewAuthor>,
    review_dismissals: Connection<RawDismissal>,
    commits: Connection<RawCommitNode<RawActivityCommit>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawThread {
    id: Option<String>,
    is_resolved: bool,
    is_outdated: bool,
    path: Option<String>,
    line: Option<i64>,
    diff_side: Option<String>,
    comments: Connection<RawThreadComment>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawThreadComment {
    id: String,
    #[serde(default)]
    author: Option<RawActor>,
    #[serde(default)]
    body: String,
    created_at: DateTime<Utc>,
    #[serde(default)]
    last_edited_at: Option<DateTime<Utc>>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    reaction_groups: Vec<RawReactionGroup>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawReactable {
    id: Option<String>,
    last_edited_at: Option<DateTime<Utc>>,
    author: Option<RawActor>,
    reaction_groups: Vec<RawReactionGroup>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawLatestReviewAuthor {
    author: Option<RawActor>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawDismissal {
    dismissal_message: Option<String>,
    review: Option<RawNodeId>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawNodeId {
    id: Option<String>,
}

/// One page of the review-thread read.
#[derive(Debug, Default)]
pub(crate) struct ThreadsPage {
    pub threads: Vec<ReviewThread>,
    pub next_cursor: Option<String>,
    pub reactions: Vec<Reaction>,
    pub reactions_by_id: HashMap<String, Vec<Reaction>>,
    pub edited_at_by_id: HashMap<String, DateTime<Utc>>,
    pub reviewers: Vec<Actor>,
    pub avatars_by_login: HashMap<String, String>,
    pub bot_logins: HashSet<String>,
    pub commits: Vec<Commit>,
    pub dismissals_by_review_id: HashMap<String, String>,
    pub next_dismissal_cursor: Option<String>,
}

fn thread_comment(comment: &RawThreadComment, viewer: Option<&str>) -> ThreadComment {
    ThreadComment {
        id: comment.id.clone(),
        author: actor(comment.author.as_ref()),
        body: comment.body.clone(),
        created_at: comment.created_at,
        edited_at: comment.last_edited_at,
        url: trimmed(comment.url.as_deref()),
        reactions: reactions(&comment.reaction_groups, viewer),
    }
}

fn dismissals(connection: &Connection<RawDismissal>) -> HashMap<String, String> {
    connection
        .nodes()
        .filter_map(|node| {
            let review = trimmed(node.review.as_ref()?.id.as_deref())?;
            Some((review, trimmed(node.dismissal_message.as_deref())?))
        })
        .collect()
}

pub(crate) fn threads_page(data: ThreadsData) -> Option<ThreadsPage> {
    let viewer = data.viewer.and_then(|v| trimmed(v.login.as_deref()));
    let viewer = viewer.as_deref();
    let pr = data.repository?.pull_request?;
    let threads = pr
        .review_threads
        .nodes()
        .filter_map(|thread| {
            let comments: Vec<&RawThreadComment> = thread.comments.nodes().collect();
            if comments.is_empty() {
                return None;
            }
            Some(ReviewThread {
                id: trimmed(thread.id.as_deref())?,
                path: trimmed(thread.path.as_deref())?,
                // An outdated thread's line has left the diff.
                line: thread.line.filter(|line| *line > 0).map(|line| line as u64),
                side: if thread
                    .diff_side
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case("LEFT"))
                {
                    DiffSide::Left
                } else {
                    DiffSide::Right
                },
                is_resolved: thread.is_resolved,
                is_outdated: thread.is_outdated,
                comments: comments.iter().map(|c| thread_comment(c, viewer)).collect(),
                comment_count: thread.comments.total_count.unwrap_or(comments.len() as u64),
                next_comments_cursor: thread.comments.next_cursor(),
            })
        })
        .collect();

    let requested: Vec<&RawReviewer> = pr
        .review_requests
        .nodes()
        .filter_map(|r| r.requested_reviewer.as_ref())
        .collect();
    let mut avatars_by_login = HashMap::new();
    let mut bot_logins = HashSet::new();
    let mut note = |actor: Option<Actor>| {
        if let Some(actor) = actor {
            if let Some(avatar) = actor.avatar_url {
                avatars_by_login.insert(actor.login.clone(), avatar);
            }
            if actor.is_bot {
                bot_logins.insert(actor.login);
            }
        }
    };
    note(actor(pr.author.as_ref()));
    for node in pr.comments.nodes().chain(pr.reviews.nodes()) {
        note(actor(node.author.as_ref()));
    }
    for reviewer in &requested {
        note(reviewer.as_actor());
    }
    for review in pr.latest_reviews.nodes() {
        note(actor(review.author.as_ref()));
    }
    for thread in pr.review_threads.nodes() {
        for comment in thread.comments.nodes() {
            note(actor(comment.author.as_ref()));
        }
    }

    // Outstanding requests, then whoever already reviewed: each person once.
    let mut reviewers: Vec<Actor> = Vec::new();
    let candidates = requested
        .iter()
        .map(|r| r.as_actor().or_else(|| r.as_team()))
        .chain(pr.latest_reviews.nodes().map(|r| actor(r.author.as_ref())));
    for candidate in candidates.flatten() {
        if !reviewers.iter().any(|r| r.login == candidate.login) {
            reviewers.push(candidate);
        }
    }

    let mut reactions_by_id = HashMap::new();
    let mut edited_at_by_id = HashMap::new();
    for node in pr.comments.nodes().chain(pr.reviews.nodes()) {
        let Some(id) = trimmed(node.id.as_deref()) else {
            continue;
        };
        if let Some(edited_at) = node.last_edited_at {
            edited_at_by_id.insert(id.clone(), edited_at);
        }
        let node_reactions = reactions(&node.reaction_groups, viewer);
        if !node_reactions.is_empty() {
            reactions_by_id.insert(id, node_reactions);
        }
    }

    let commits = pr
        .commits
        .nodes()
        .filter_map(|node| {
            let commit = &node.commit;
            Some(Commit {
                oid: trimmed(commit.oid.as_deref())?,
                message_headline: commit.message_headline.clone(),
                committed_date: commit.committed_date?,
                additions: None,
                deletions: None,
                authors: commit
                    .authors
                    .nodes()
                    .filter_map(|author| {
                        let login = author
                            .user
                            .as_ref()
                            .and_then(|u| trimmed(u.login.as_deref()))
                            .or_else(|| trimmed(author.name.as_deref()))?;
                        Some(Actor {
                            login,
                            name: trimmed(author.name.as_deref()),
                            avatar_url: trimmed(author.avatar_url.as_deref()),
                            is_bot: false,
                        })
                    })
                    .collect(),
            })
        })
        .collect();

    Some(ThreadsPage {
        threads,
        next_cursor: pr.review_threads.next_cursor(),
        reactions: reactions(&pr.reaction_groups, viewer),
        reactions_by_id,
        edited_at_by_id,
        reviewers,
        avatars_by_login,
        bot_logins,
        commits,
        dismissals_by_review_id: dismissals(&pr.review_dismissals),
        next_dismissal_cursor: pr.review_dismissals.next_cursor(),
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct DismissalsData {
    repository: Option<RawDismissalsRepository>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawDismissalsRepository {
    pull_request: Option<RawDismissalsPullRequest>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawDismissalsPullRequest {
    timeline_items: Connection<RawDismissal>,
}

pub(crate) fn dismissals_page(
    data: DismissalsData,
) -> Option<(HashMap<String, String>, Option<String>)> {
    let items = data.repository?.pull_request?.timeline_items;
    Some((dismissals(&items), items.next_cursor()))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ThreadCommentsData {
    viewer: Option<RawLogin>,
    repository: Option<RawSubjectRepository>,
    node: Option<RawThreadNode>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RawSubjectRepository {
    pub pull_request: Option<RawNodeIdPublic>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct RawNodeIdPublic {
    pub id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawThreadNode {
    #[serde(rename = "__typename")]
    typename: Option<String>,
    pull_request: Option<RawNodeId>,
    comments: Connection<RawThreadComment>,
}

/// More comments of one thread, and whether the thread belongs to the
/// pull request the caller named.
pub(crate) fn thread_comments_page(data: ThreadCommentsData) -> (bool, ThreadCommentsPage) {
    let viewer = data.viewer.and_then(|v| trimmed(v.login.as_deref()));
    let expected = data
        .repository
        .and_then(|r| r.pull_request)
        .and_then(|pr| pr.id);
    let thread = data
        .node
        .filter(|node| node.typename.as_deref() == Some("PullRequestReviewThread"));
    let actual = thread
        .as_ref()
        .and_then(|t| t.pull_request.as_ref())
        .and_then(|pr| pr.id.clone());
    let belongs = expected.is_some() && expected == actual;
    let page = ThreadCommentsPage {
        comments: thread
            .as_ref()
            .map(|t| {
                t.comments
                    .nodes()
                    .map(|c| thread_comment(c, viewer.as_deref()))
                    .collect()
            })
            .unwrap_or_default(),
        next_cursor: thread.and_then(|t| t.comments.next_cursor()),
    };
    (belongs, page)
}

// --- Combining -------------------------------------------------------------

/// The activity read, all pages.
pub(crate) struct ActivityRead {
    pub author: Option<Actor>,
    pub comments: Vec<Comment>,
    pub commits: Vec<Commit>,
}

/// The review-thread read, all pages.
#[derive(Debug, Default)]
pub(crate) struct ThreadsRead {
    pub first: ThreadsPage,
    pub threads: Vec<ReviewThread>,
    pub dismissals_by_review_id: HashMap<String, String>,
    /// More threads than were read.
    pub threads_truncated: bool,
}

impl ThreadsRead {
    /// What stands in when the thread read failed: nothing, marked as cut.
    pub fn unavailable() -> Self {
        Self {
            threads_truncated: true,
            ..Self::default()
        }
    }

    fn comment_count(&self) -> u64 {
        self.threads.iter().map(|t| t.comment_count).sum()
    }

    fn comments_truncated(&self) -> bool {
        self.threads_truncated
            || self
                .threads
                .iter()
                .any(|t| t.next_comments_cursor.is_some())
    }
}

/// A body that shows nothing once HTML comments are gone, as bots leave.
fn renders_empty(body: &str) -> bool {
    let mut rest = body;
    let mut visible = String::new();
    while let Some(start) = rest.find("<!--") {
        visible.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                rest = &rest[start..];
                break;
            }
        }
    }
    visible.push_str(rest);
    visible.trim().is_empty()
}

/// Fills in what one read knows about another's actors: avatars, bots.
fn with_avatar(actor: Option<Actor>, threads: &ThreadsPage, host: &str) -> Option<Actor> {
    actor.map(|mut actor| {
        if threads.bot_logins.contains(&actor.login) {
            actor.is_bot = true;
        }
        if actor.avatar_url.is_none() {
            actor.avatar_url = threads
                .avatars_by_login
                .get(&actor.login)
                .cloned()
                .or_else(|| login_avatar_url(&actor.login, host));
        }
        actor
    })
}

/// Combines the activity and thread reads into the conversation.
pub(crate) fn assemble(activity: ActivityRead, threads: ThreadsRead, host: &str) -> Activity {
    let page = &threads.first;
    let comment_count = activity.comments.len() as u64 + threads.comment_count();
    let comments_truncated = threads.comments_truncated();
    let thread_remarks = threads.threads.iter().flat_map(|thread| {
        thread.comments.iter().map(|comment| Comment {
            id: comment.id.clone(),
            kind: CommentKind::ReviewComment,
            author: comment.author.clone(),
            body: comment.body.clone(),
            created_at: comment.created_at,
            edited_at: comment.edited_at,
            url: comment.url.clone(),
            path: Some(thread.path.clone()),
            review_state: None,
            reactions: comment.reactions.clone(),
        })
    });
    let mut comments: Vec<Comment> = activity
        .comments
        .into_iter()
        .map(|mut comment| {
            // Dismissal reasons live on timeline events.
            if comment.review_state == Some(ReviewState::Dismissed)
                && renders_empty(&comment.body)
                && let Some(message) = threads.dismissals_by_review_id.get(&comment.id)
            {
                comment.body = message.clone();
            }
            comment.reactions = page
                .reactions_by_id
                .get(&comment.id)
                .cloned()
                .unwrap_or_default();
            comment
        })
        .chain(thread_remarks)
        .map(|mut comment| {
            comment.author = with_avatar(comment.author, page, host);
            if let Some(edited_at) = page.edited_at_by_id.get(&comment.id) {
                comment.edited_at = Some(*edited_at);
            }
            comment
        })
        .collect();
    comments.sort_by_key(|c| c.created_at);

    // The thread read's commits carry avatars; the activity's carry stats.
    let stats: HashMap<&str, (Option<u64>, Option<u64>)> = activity
        .commits
        .iter()
        .map(|c| (c.oid.as_str(), (c.additions, c.deletions)))
        .collect();
    let commits = if page.commits.is_empty() {
        &activity.commits
    } else {
        &page.commits
    };
    let commits = commits
        .iter()
        .map(|commit| {
            let (additions, deletions) =
                stats.get(commit.oid.as_str()).copied().unwrap_or_default();
            Commit {
                additions: additions.or(commit.additions),
                deletions: deletions.or(commit.deletions),
                authors: commit
                    .authors
                    .iter()
                    .map(|a| with_avatar(Some(a.clone()), page, host).unwrap_or_else(|| a.clone()))
                    .collect(),
                ..commit.clone()
            }
        })
        .collect();

    let review_threads = threads
        .threads
        .iter()
        .map(|thread| ReviewThread {
            comments: thread
                .comments
                .iter()
                .map(|c| ThreadComment {
                    author: with_avatar(c.author.clone(), page, host),
                    ..c.clone()
                })
                .collect(),
            ..thread.clone()
        })
        .collect();

    Activity {
        author: with_avatar(with_fallback_avatar(activity.author, host), page, host),
        reviewers: page.reviewers.clone(),
        comments,
        comment_count,
        comments_truncated,
        review_threads,
        review_threads_truncated: threads.threads_truncated,
        commits,
        reactions: page.reactions.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn activity_fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/pull_request_activity.json"
        ))
        .unwrap()
    }

    fn threads_fixture() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/review_threads.json")).unwrap()
    }

    fn activity(data: Value) -> ActivityPage {
        activity_page(serde_json::from_value(data).unwrap()).unwrap()
    }

    fn threads(data: Value) -> ThreadsPage {
        threads_page(serde_json::from_value(data).unwrap()).unwrap()
    }

    #[test]
    fn merges_reviews_with_comments_in_time_order_and_keeps_a_bodyless_approval() {
        let page = activity(activity_fixture()["data"].clone());
        let remarks: Vec<_> = page
            .remarks
            .iter()
            .map(|c| (c.id.as_str(), c.kind, c.review_state))
            .collect();
        assert_eq!(
            remarks,
            [
                ("IC_1", CommentKind::IssueComment, None),
                (
                    "PRR_approve",
                    CommentKind::Review,
                    Some(ReviewState::Approved)
                ),
                ("IC_2", CommentKind::IssueComment, None),
                (
                    "PRR_dismissed",
                    CommentKind::Review,
                    Some(ReviewState::Dismissed)
                ),
            ]
        );
        assert_eq!(page.next_comments_cursor.as_deref(), Some("comments-2"));
        assert_eq!(page.next_reviews_cursor, None);
    }

    #[test]
    fn selects_stats_only_for_non_merge_commits_and_reads_unlinked_authors() {
        let page = activity(activity_fixture()["data"].clone());
        assert_eq!(page.non_merge_commit_ids, ["C_first"]);
        assert_eq!(page.commits.len(), 2);
        let authors: Vec<_> = page.commits[0]
            .authors
            .iter()
            .map(|a| a.login.as_str())
            .collect();
        assert_eq!(authors, ["stippi", "Co Author"]);
        assert_eq!(page.author.as_ref().unwrap().login, "contributor");
    }

    #[test]
    fn reads_commit_stats_of_commits_only() {
        let stats = commit_stats(
            serde_json::from_value(json!({"nodes": [
                {"__typename": "Commit", "oid": "abc", "additions": 3, "deletions": 1},
                {"__typename": "Blob"},
                null
            ]}))
            .unwrap(),
        );
        assert_eq!(stats.len(), 1);
        assert_eq!(stats["abc"], (3, 1));
    }

    #[test]
    fn anchors_threads_to_line_and_side_and_leaves_outdated_ones_unanchored() {
        let page = threads(threads_fixture()["data"].clone());
        assert_eq!(page.threads.len(), 2);
        let first = &page.threads[0];
        assert_eq!(first.path, "src/lib.rs");
        assert_eq!(first.line, Some(12));
        assert_eq!(first.side, DiffSide::Left);
        assert!(first.is_resolved);
        assert_eq!(first.comments.len(), 2);
        assert_eq!(first.comment_count, 12);
        assert_eq!(
            first.next_comments_cursor.as_deref(),
            Some("thread-1-comments")
        );
        let second = &page.threads[1];
        assert_eq!(second.line, None);
        assert!(second.is_outdated);
        assert_eq!(second.side, DiffSide::Right);
        assert_eq!(second.next_comments_cursor, None);
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn leaves_the_viewer_out_of_reactors_while_counting_them() {
        let page = threads(threads_fixture()["data"].clone());
        assert_eq!(
            page.reactions,
            [Reaction {
                content: ReactionContent::ThumbsUp,
                count: 14,
                actors: vec!["octocat".into()],
                viewer_has_reacted: true,
            }]
        );
        let comment_reactions = &page.reactions_by_id["IC_1"];
        assert_eq!(comment_reactions[0].content, ReactionContent::Heart);
        assert_eq!(comment_reactions[0].count, 1);
    }

    #[test]
    fn lists_reviewers_once_teams_by_org_slug_and_notes_bots() {
        let page = threads(threads_fixture()["data"].clone());
        let reviewers: Vec<_> = page.reviewers.iter().map(|r| r.login.as_str()).collect();
        assert_eq!(reviewers, ["octocat", "acme/core", "review-bot"]);
        assert!(page.bot_logins.contains("review-bot"));
        assert_eq!(
            page.dismissals_by_review_id["PRR_dismissed"],
            "Outdated after the rewrite"
        );
        assert_eq!(page.next_dismissal_cursor, None);
    }

    #[test]
    fn combines_activity_and_threads_into_one_conversation() {
        let activity_page = activity(activity_fixture()["data"].clone());
        let threads_page = threads(threads_fixture()["data"].clone());
        let read = ThreadsRead {
            threads: threads_page.threads.clone(),
            dismissals_by_review_id: threads_page.dismissals_by_review_id.clone(),
            threads_truncated: false,
            first: threads_page,
        };
        let mut stats = activity_page.commits.clone();
        stats[0].additions = Some(5);
        stats[0].deletions = Some(2);
        let assembled = assemble(
            ActivityRead {
                author: activity_page.author,
                comments: activity_page.remarks,
                commits: stats,
            },
            read,
            "github.com",
        );
        let ids: Vec<_> = assembled.comments.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "IC_1",
                "PRRC_1",
                "PRR_approve",
                "PRRC_2",
                "IC_2",
                "PRRC_3",
                "PRR_dismissed"
            ]
        );
        let dismissed = assembled.comments.last().unwrap();
        assert_eq!(dismissed.body, "Outdated after the rewrite");
        let first = &assembled.comments[0];
        assert_eq!(first.reactions[0].content, ReactionContent::Heart);
        assert!(first.edited_at.is_some());
        // The approval's author gets the avatar only the thread read carried.
        let approver = assembled.comments[2].author.as_ref().unwrap();
        assert!(approver.is_bot);
        assert_eq!(
            approver.avatar_url.as_deref(),
            Some("https://avatars.githubusercontent.com/in/9")
        );
        assert_eq!(assembled.comment_count, 4 + 12 + 1);
        assert!(assembled.comments_truncated);
        assert_eq!(assembled.commits[0].additions, Some(5));
        assert_eq!(
            assembled.commits[0].authors[0].avatar_url.as_deref(),
            Some("https://avatars.githubusercontent.com/u/1?v=4")
        );
    }

    #[test]
    fn tells_whether_a_thread_belongs_to_the_pull_request() {
        let data = |thread_pr: &str| {
            serde_json::from_value::<ThreadCommentsData>(json!({
                "viewer": {"login": "me"},
                "repository": {"pullRequest": {"id": "PR_1"}},
                "node": {
                    "__typename": "PullRequestReviewThread",
                    "pullRequest": {"id": thread_pr},
                    "comments": {
                        "pageInfo": {"hasNextPage": false, "endCursor": "x"},
                        "nodes": [{"id": "C", "body": "hi", "createdAt": "2026-10-30T10:00:00Z"}]
                    }
                }
            }))
            .unwrap()
        };
        let (belongs, page) = thread_comments_page(data("PR_1"));
        assert!(belongs);
        assert_eq!(page.comments.len(), 1);
        assert_eq!(page.next_cursor, None);
        assert!(!thread_comments_page(data("PR_2")).0);
    }

    #[test]
    fn finds_bodies_that_render_empty() {
        assert!(renders_empty("  <!-- bot marker -->\n"));
        assert!(!renders_empty("<!-- x --> text"));
        assert!(!renders_empty("text <!-- unterminated"));
    }
}
