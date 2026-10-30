//! The pull request's history as the Timeline tab lists it, ported from
//! T3 Code (`buildPullRequestTimeline`, `groupPullRequestTimelineConversations`):
//! opened, commits, the conversation (threads at their first comment's
//! time), merged or closed; plain comments next to each other fold into one
//! row.

use super::super::conversation::logic::{ConversationItem, ReviewOutcome, ThreadItem};
use chrono::{DateTime, Utc};
use github::model::{Activity, Actor, Comment, Commit, Detail};

#[derive(Clone, Debug, PartialEq)]
pub enum TimelineEvent {
    Opened {
        at: DateTime<Utc>,
        author: Option<Actor>,
    },
    Commit(Commit),
    /// A comment or review outside a thread.
    Comment(Comment),
    Thread(Box<ThreadItem>),
    Merged {
        at: DateTime<Utc>,
    },
    Closed {
        at: DateTime<Utc>,
    },
}

impl TimelineEvent {
    pub fn at(&self) -> DateTime<Utc> {
        match self {
            TimelineEvent::Opened { at, .. }
            | TimelineEvent::Merged { at }
            | TimelineEvent::Closed { at } => *at,
            TimelineEvent::Commit(commit) => commit.committed_date,
            TimelineEvent::Comment(comment) => comment.created_at,
            TimelineEvent::Thread(thread) => thread.root.created_at,
        }
    }

    /// A comment without a verdict, which folds with its neighbours.
    fn plain_comment(&self) -> Option<&Comment> {
        match self {
            TimelineEvent::Comment(comment)
                if ReviewOutcome::of(comment.review_state).is_none() =>
            {
                Some(comment)
            }
            _ => None,
        }
    }
}

/// The events, newest first. `conversation` is the conversation's items
/// ([`conversation_items`](super::super::conversation::logic::conversation_items)).
/// Merged wins over closed: GitHub sets both on a merge.
pub fn build_timeline(
    detail: &Detail,
    activity: &Activity,
    conversation: &[ConversationItem],
) -> Vec<TimelineEvent> {
    let opened = TimelineEvent::Opened {
        at: detail.created_at,
        author: detail.author.clone().or_else(|| activity.author.clone()),
    };
    let ended = match (detail.merged_at, detail.closed_at) {
        (Some(at), _) => Some(TimelineEvent::Merged { at }),
        (None, Some(at)) => Some(TimelineEvent::Closed { at }),
        (None, None) => None,
    };
    let mut events: Vec<TimelineEvent> = std::iter::once(opened)
        .chain(activity.commits.iter().cloned().map(TimelineEvent::Commit))
        .chain(conversation.iter().map(|item| match item {
            ConversationItem::Comment(comment) => TimelineEvent::Comment(comment.clone()),
            ConversationItem::Thread(thread) => TimelineEvent::Thread(Box::new(thread.clone())),
        }))
        .chain(ended)
        .collect();
    // Stable: what happened at the same moment keeps the order above.
    events.sort_by_key(|event| std::cmp::Reverse(event.at()));
    events
}

/// One row of the rail.
#[derive(Clone, Debug, PartialEq)]
pub enum TimelineRow {
    Event(Box<TimelineEvent>),
    /// Plain comments next to each other.
    Comments(Vec<Comment>),
}

/// `events` (newest first) in the shown order, neighbouring plain comments
/// folded.
pub fn timeline_rows(events: Vec<TimelineEvent>, newest_first: bool) -> Vec<TimelineRow> {
    let ordered: Box<dyn Iterator<Item = TimelineEvent>> = if newest_first {
        Box::new(events.into_iter())
    } else {
        Box::new(events.into_iter().rev())
    };
    let mut rows: Vec<TimelineRow> = Vec::new();
    for event in ordered {
        match (event.plain_comment(), rows.last_mut()) {
            (Some(comment), Some(TimelineRow::Comments(group))) => group.push(comment.clone()),
            (Some(comment), _) => rows.push(TimelineRow::Comments(vec![comment.clone()])),
            (None, _) => rows.push(TimelineRow::Event(Box::new(event))),
        }
    }
    rows
}

/// The authors of `comments`, each once, in order.
pub fn distinct_authors(comments: &[Comment]) -> Vec<Actor> {
    let mut authors: Vec<Actor> = Vec::new();
    for author in comments.iter().filter_map(|c| c.author.as_ref()) {
        if !authors.iter().any(|a| a.login == author.login) {
            authors.push(author.clone());
        }
    }
    authors
}

#[cfg(test)]
mod tests {
    use super::super::super::conversation::fixtures::{comment, commit, review, thread};
    use super::super::super::conversation::logic::{conversation_items, thread_item};
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::{
        actor, detail, detail_with, hours_ago,
    };
    use github::model::{PullRequestState, ReviewState};

    fn activity(comments: Vec<Comment>, threads: Vec<github::model::ReviewThread>) -> Activity {
        Activity {
            author: Some(actor("ann")),
            reviewers: Vec::new(),
            comment_count: comments.len() as u64,
            comments,
            comments_truncated: false,
            review_threads: threads,
            review_threads_truncated: false,
            commits: vec![commit("c1", hours_ago(17)), commit("c2", hours_ago(12))],
            reactions: Vec::new(),
        }
    }

    fn label(event: &TimelineEvent) -> String {
        match event {
            TimelineEvent::Opened { .. } => "opened".into(),
            TimelineEvent::Commit(c) => format!("commit {}", c.oid),
            TimelineEvent::Comment(c) => format!("comment {}", c.id),
            TimelineEvent::Thread(t) => format!("thread {}", t.thread.id),
            TimelineEvent::Merged { .. } => "merged".into(),
            TimelineEvent::Closed { .. } => "closed".into(),
        }
    }

    fn row_label(row: &TimelineRow) -> String {
        match row {
            TimelineRow::Event(event) => label(event),
            TimelineRow::Comments(comments) => format!(
                "comments {}",
                comments
                    .iter()
                    .map(|c| c.id.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }

    #[test]
    fn events_run_newest_first_and_merged_wins_over_closed() {
        let detail = detail_with(290, |d| {
            d.created_at = hours_ago(18);
            d.state = PullRequestState::Merged;
            d.merged_at = Some(hours_ago(9));
            d.closed_at = Some(hours_ago(9));
        });
        let activity = activity(
            vec![comment("a", "bob", hours_ago(15))],
            vec![thread("t", &[("t-root", hours_ago(11))])],
        );
        let conversation = conversation_items(&activity.comments, &activity.review_threads);
        let events = build_timeline(&detail, &activity, &conversation);
        let labels: Vec<_> = events.iter().map(label).collect();
        assert_eq!(
            labels,
            [
                "merged",
                "thread t",
                "commit c2",
                "comment a",
                "commit c1",
                "opened"
            ]
        );
    }

    #[test]
    fn a_closed_pull_request_ends_closed() {
        let closed = detail_with(290, |d| {
            d.state = PullRequestState::Closed;
            d.closed_at = Some(hours_ago(1));
        });
        let events = build_timeline(&closed, &activity(Vec::new(), Vec::new()), &[]);
        assert_eq!(label(&events[0]), "closed");
        let open = build_timeline(&detail(290), &activity(Vec::new(), Vec::new()), &[]);
        assert!(open.iter().all(|e| !matches!(
            e,
            TimelineEvent::Merged { .. } | TimelineEvent::Closed { .. }
        )));
    }

    #[test]
    fn neighbouring_plain_comments_fold() {
        let events = vec![
            TimelineEvent::Comment(comment("c4", "ann", hours_ago(1))),
            TimelineEvent::Comment(review("r3", "bob", ReviewState::Commented, hours_ago(2))),
            TimelineEvent::Comment(review("v2", "cid", ReviewState::Approved, hours_ago(3))),
            TimelineEvent::Comment(comment("c1", "ann", hours_ago(4))),
            TimelineEvent::Thread(Box::new(
                thread_item(&thread("t", &[("x", hours_ago(5))]), &Default::default()).unwrap(),
            )),
            TimelineEvent::Comment(comment("c0", "ann", hours_ago(6))),
            TimelineEvent::Comment(review("d", "dan", ReviewState::Dismissed, hours_ago(7))),
        ];
        let rows: Vec<_> = timeline_rows(events.clone(), true)
            .iter()
            .map(row_label)
            .collect();
        assert_eq!(
            rows,
            [
                "comments c4,r3",
                "comment v2",
                "comments c1",
                "thread t",
                "comments c0",
                "comment d"
            ]
        );
        let oldest_first: Vec<_> = timeline_rows(events, false).iter().map(row_label).collect();
        assert_eq!(oldest_first[0], "comment d");
        assert_eq!(oldest_first[5], "comments r3,c4");
    }

    #[test]
    fn group_authors_count_once() {
        let comments = [
            comment("a", "ann", hours_ago(3)),
            comment("b", "bob", hours_ago(2)),
            comment("c", "ann", hours_ago(1)),
        ];
        let logins: Vec<_> = distinct_authors(&comments)
            .into_iter()
            .map(|a| a.login)
            .collect();
        assert_eq!(logins, ["ann", "bob"]);
    }
}
