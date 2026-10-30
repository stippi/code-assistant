//! The conversation of a pull request as the Summary and Timeline tabs show
//! it, ported from T3 Code (`pullRequestDetail.logic.ts`): loose comments
//! and review threads in one sequence, the comment filter, paging, the
//! reviewers' latest verdicts and what a collapsed card previews.

use chrono::{DateTime, Utc};
use github::model::{
    Actor, Comment, CommentKind, Commit, ReviewState, ReviewThread, ThreadComment,
};
use gpui_kit::{ListOffset, ListState, px};
use std::collections::{HashMap, HashSet};

/// `body` without its surrounding whitespace; `None` when nothing but
/// whitespace and HTML comments (bots' bookkeeping markers) is left.
pub fn visible_body(body: &str) -> Option<&str> {
    let mut rest = body;
    while let Some(start) = rest.find("<!--") {
        if !rest[..start].trim().is_empty() {
            return Some(body.trim());
        }
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            // An unclosed comment is text.
            None => return Some(body.trim()),
        }
    }
    (!rest.trim().is_empty()).then(|| body.trim())
}

/// What a review says by its state alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewOutcome {
    Approved,
    ChangesRequested,
    Dismissed,
}

impl ReviewOutcome {
    pub fn of(state: Option<ReviewState>) -> Option<Self> {
        match state? {
            ReviewState::Approved => Some(ReviewOutcome::Approved),
            ReviewState::ChangesRequested => Some(ReviewOutcome::ChangesRequested),
            ReviewState::Dismissed => Some(ReviewOutcome::Dismissed),
            ReviewState::Commented | ReviewState::Pending => None,
        }
    }

    /// An approval or a change request; a dismissed review no longer counts.
    pub fn is_verdict(self) -> bool {
        self != ReviewOutcome::Dismissed
    }

    pub fn label(self) -> &'static str {
        match self {
            ReviewOutcome::Approved => "Approved",
            ReviewOutcome::ChangesRequested => "Changes requested",
            ReviewOutcome::Dismissed => "Review dismissed",
        }
    }

    /// Said of a verdict given before the newest commit.
    pub fn stale_label(self) -> String {
        format!("{} earlier changes", self.label())
    }
}

/// A review thread with its comments in the conversation's form.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadItem {
    pub thread: ReviewThread,
    /// The comment that opened the thread.
    pub root: Comment,
    pub replies: Vec<Comment>,
    /// Comments of the thread that were not read.
    pub more_replies: u64,
    /// More comments can be read (counted or behind a cursor).
    pub has_more_replies: bool,
    /// Where the thread is on GitHub.
    pub url: Option<String>,
}

/// One entry of the conversation: a comment on its own or a review thread.
#[derive(Clone, Debug, PartialEq)]
pub enum ConversationItem {
    Comment(Comment),
    Thread(ThreadItem),
}

impl ConversationItem {
    pub fn id(&self) -> &str {
        match self {
            ConversationItem::Comment(c) => &c.id,
            ConversationItem::Thread(t) => &t.thread.id,
        }
    }

    /// The comment the item starts with.
    pub fn lead(&self) -> &Comment {
        match self {
            ConversationItem::Comment(c) => c,
            ConversationItem::Thread(t) => &t.root,
        }
    }

    pub fn thread(&self) -> Option<&ThreadItem> {
        match self {
            ConversationItem::Comment(_) => None,
            ConversationItem::Thread(t) => Some(t),
        }
    }

    /// Every comment of the item that was read.
    pub fn comments(&self) -> impl Iterator<Item = &Comment> {
        let replies = self.thread().map_or(&[][..], |t| &t.replies[..]);
        std::iter::once(self.lead()).chain(replies)
    }
}

/// A thread comment as the conversation holds it.
pub fn thread_comment(comment: &ThreadComment, path: &str) -> Comment {
    Comment {
        id: comment.id.clone(),
        kind: CommentKind::ReviewComment,
        author: comment.author.clone(),
        body: comment.body.clone(),
        created_at: comment.created_at,
        edited_at: comment.edited_at,
        url: comment.url.clone(),
        path: Some(path.to_owned()),
        review_state: None,
        reactions: comment.reactions.clone(),
    }
}

/// A thread with the conversation's copies of its comments (they carry
/// what edits and reactions need), oldest first.
pub fn thread_item(thread: &ReviewThread, by_id: &HashMap<&str, &Comment>) -> Option<ThreadItem> {
    let mut ordered: Vec<Comment> = thread
        .comments
        .iter()
        .map(|c| match by_id.get(c.id.as_str()) {
            Some(known) => (*known).clone(),
            None => thread_comment(c, &thread.path),
        })
        .collect();
    ordered.sort_by_key(|c| c.created_at);
    let mut comments = ordered.into_iter();
    let root = comments.next()?;
    let replies: Vec<Comment> = comments.collect();
    let read = 1 + replies.len() as u64;
    let more_replies = thread.comment_count.saturating_sub(read);
    let url = std::iter::once(&root)
        .chain(&replies)
        .find_map(|c| c.url.clone());
    Some(ThreadItem {
        thread: thread.clone(),
        root,
        replies,
        more_replies,
        has_more_replies: more_replies > 0 || thread.next_comments_cursor.is_some(),
        url,
    })
}

/// Loose comments in their order, each thread inserted at the time of its
/// first comment.
pub fn conversation_items(comments: &[Comment], threads: &[ReviewThread]) -> Vec<ConversationItem> {
    let by_id: HashMap<&str, &Comment> = comments.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut claimed: HashSet<String> = HashSet::new();
    let mut thread_items: Vec<ThreadItem> = threads
        .iter()
        .filter_map(|thread| thread_item(thread, &by_id))
        .inspect(|item| {
            claimed.extend(
                std::iter::once(&item.root)
                    .chain(&item.replies)
                    .map(|c| c.id.clone()),
            );
        })
        .collect();
    thread_items.sort_by_key(|t| t.root.created_at);
    let mut threads = thread_items.into_iter().peekable();
    let mut items = Vec::with_capacity(comments.len());
    for comment in comments.iter().filter(|c| !claimed.contains(&c.id)) {
        while let Some(thread) = threads.next_if(|t| t.root.created_at < comment.created_at) {
            items.push(ConversationItem::Thread(thread));
        }
        items.push(ConversationItem::Comment(comment.clone()));
    }
    items.extend(threads.map(ConversationItem::Thread));
    items
}

/// Settled: in a resolved thread, or a dismissed review.
pub fn is_finished(comment: &Comment, thread: Option<&ReviewThread>) -> bool {
    thread.is_some_and(|t| t.is_resolved)
        || ReviewOutcome::of(comment.review_state) == Some(ReviewOutcome::Dismissed)
}

/// What the Comments section shows besides people's open remarks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommentFilter {
    pub bots: bool,
    /// Resolved threads and dismissed reviews.
    pub resolved: bool,
}

impl CommentFilter {
    /// An item shows when any of its comments passes, which keeps a bot's
    /// finding beside a person's reply. A bot's approval or change request
    /// is a verdict, never chatter.
    pub fn shows(self, item: &ConversationItem) -> bool {
        let thread = item.thread().map(|t| &t.thread);
        item.comments().any(|comment| {
            let is_bot = comment.author.as_ref().is_some_and(|a| a.is_bot);
            let is_verdict =
                ReviewOutcome::of(comment.review_state).is_some_and(ReviewOutcome::is_verdict);
            (self.bots || !is_bot || is_verdict) && (self.resolved || !is_finished(comment, thread))
        })
    }
}

/// The indices of `items` the filter shows.
pub fn filter_items(items: &[ConversationItem], filter: CommentFilter) -> Vec<usize> {
    (0..items.len())
        .filter(|&i| filter.shows(&items[i]))
        .collect()
}

/// Comments in `items`, read ones.
pub fn comment_count<'a>(items: impl IntoIterator<Item = &'a ConversationItem>) -> usize {
    items.into_iter().map(|item| item.comments().count()).sum()
}

/// The newest `count` of `len` entries: the range shown and how many older
/// ones stay hidden.
pub fn page(len: usize, count: usize) -> (std::ops::Range<usize>, usize) {
    let start = len.saturating_sub(count);
    (start..len, start)
}

pub fn newest_commit_at(commits: &[Commit]) -> Option<DateTime<Utc>> {
    commits.iter().map(|c| c.committed_date).max()
}

/// A verdict given before the newest commit reviewed older changes.
pub fn is_verdict_stale(at: DateTime<Utc>, newest_commit_at: Option<DateTime<Utc>>) -> bool {
    newest_commit_at.is_some_and(|commit_at| at < commit_at)
}

/// One reviewer's latest verdict.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewerOutcome {
    pub actor: Option<Actor>,
    pub outcome: ReviewOutcome,
    pub at: DateTime<Utc>,
    pub stale: bool,
}

/// Each reviewer's latest verdict; reviewers whose latest review was
/// dismissed have none.
pub fn latest_review_outcomes(comments: &[Comment], commits: &[Commit]) -> Vec<ReviewerOutcome> {
    let newest = newest_commit_at(commits);
    let mut order: Vec<String> = Vec::new();
    let mut latest: HashMap<String, ReviewerOutcome> = HashMap::new();
    for comment in comments {
        let Some(outcome) = ReviewOutcome::of(comment.review_state) else {
            continue;
        };
        let key = comment.author.as_ref().map_or_else(
            || format!("ghost:{}", comment.id),
            |a| a.login.to_lowercase(),
        );
        // Hosts can return reviews out of order.
        if latest
            .get(&key)
            .is_some_and(|current| current.at > comment.created_at)
        {
            continue;
        }
        if !latest.contains_key(&key) {
            order.push(key.clone());
        }
        latest.insert(
            key,
            ReviewerOutcome {
                actor: comment.author.clone(),
                outcome,
                at: comment.created_at,
                stale: is_verdict_stale(comment.created_at, newest),
            },
        );
    }
    order
        .into_iter()
        .filter_map(|key| latest.remove(&key))
        .filter(|entry| entry.outcome.is_verdict())
        .collect()
}

/// A reviewer as the Reviewers row shows them.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewerEntry {
    pub actor: Option<Actor>,
    pub outcome: Option<ReviewOutcome>,
    pub stale: bool,
}

/// The requested reviewers with their verdicts, then everyone else who gave
/// one (GitHub drops a request once it is answered). Logins match without
/// regard to case.
pub fn reviewer_entries(requested: &[Actor], outcomes: &[ReviewerOutcome]) -> Vec<ReviewerEntry> {
    let outcome_of = |login: &str| {
        outcomes.iter().find(|o| {
            o.actor
                .as_ref()
                .is_some_and(|a| a.login.eq_ignore_ascii_case(login))
        })
    };
    let requested_entries = requested.iter().map(|actor| {
        let outcome = outcome_of(&actor.login);
        ReviewerEntry {
            actor: Some(actor.clone()),
            outcome: outcome.map(|o| o.outcome),
            stale: outcome.is_some_and(|o| o.stale),
        }
    });
    let others = outcomes
        .iter()
        .filter(|o| {
            !requested.iter().any(|actor| {
                o.actor
                    .as_ref()
                    .is_some_and(|a| a.login.eq_ignore_ascii_case(&actor.login))
            })
        })
        .map(|o| ReviewerEntry {
            actor: o.actor.clone(),
            outcome: Some(o.outcome),
            stale: o.stale,
        });
    requested_entries.chain(others).collect()
}

/// `loaded` appended to `base`, without the comments already there.
pub fn merge_thread_comments(
    base: &[ThreadComment],
    loaded: &[ThreadComment],
) -> Vec<ThreadComment> {
    let mut seen: HashSet<&str> = base.iter().map(|c| c.id.as_str()).collect();
    let mut merged = base.to_vec();
    merged.extend(
        loaded
            .iter()
            .filter(|c| seen.insert(c.id.as_str()))
            .cloned(),
    );
    merged
}

/// The viewer may rewrite their own issue and review comments; GitHub has
/// no mutation for a review's summary.
pub fn can_edit(comment: &Comment, viewer: Option<&str>) -> bool {
    comment.kind != CommentKind::Review
        && viewer.is_some_and(|viewer| {
            comment
                .author
                .as_ref()
                .is_some_and(|a| a.login.trim().eq_ignore_ascii_case(viewer.trim()))
        })
}

/// What changed between two row lists, as a splice for a `ListState`: the
/// range of `old` that `count` rows of `new` replace (common rows at both
/// ends stay measured and scrolled where they are). `None` when equal.
pub fn list_splice<T: PartialEq>(old: &[T], new: &[T]) -> Option<(std::ops::Range<usize>, usize)> {
    if old == new {
        return None;
    }
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    Some((prefix..old.len() - suffix, new.len() - prefix - suffix))
}

/// Tell `list` that its rows went from `old` to `new`. A list scrolled to
/// its very top stays there: GPUI keeps the row seen first in view, so rows
/// put in front of it (the first rows of an empty list among them) would
/// scroll it down past them.
pub fn splice_list<T: PartialEq>(list: &ListState, old: &[T], new: &[T]) {
    let Some((range, count)) = list_splice(old, new) else {
        return;
    };
    let top = list.logical_scroll_top();
    let at_top = top.item_ix == 0 && top.offset_in_item == px(0.);
    list.splice(range, count);
    if at_top {
        list.scroll_to(ListOffset::default());
    }
}

/// The viewer may rewrite the description of their own pull request, or
/// of any they may merge.
pub fn can_edit_description(detail: &github::model::Detail, viewer: Option<&str>) -> bool {
    let is_author = viewer.is_some_and(|viewer| {
        detail
            .author
            .as_ref()
            .is_some_and(|a| a.login.eq_ignore_ascii_case(viewer.trim()))
    });
    is_author
        || detail
            .viewer_permissions
            .actions
            .contains(&github::model::PullRequestAction::Merge)
}

/// One line of plain text for a collapsed card: Markdown marks, alert
/// markers, link targets and HTML comments left out.
pub fn preview_text(body: &str) -> String {
    let mut text = String::with_capacity(body.len());
    for line in strip_html_comments(body).lines() {
        if is_alert_marker(line) {
            continue;
        }
        let line = line.trim_start_matches(|c: char| c.is_whitespace() || "> #*-".contains(c));
        text.push_str(&unlink(line));
        text.push(' ');
    }
    text.retain(|c| c != '*' && c != '`');
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_html_comments(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("<!--") {
        let Some(end) = rest[start..].find("-->") else {
            break;
        };
        out.push_str(&rest[..start]);
        rest = &rest[start + end + 3..];
    }
    out.push_str(rest);
    out
}

/// `> [!NOTE]` alone on its line.
fn is_alert_marker(line: &str) -> bool {
    let line = line.trim();
    let line = line.strip_prefix('>').unwrap_or(line).trim_start();
    line.strip_prefix("[!")
        .and_then(|rest| rest.strip_suffix(']'))
        .is_some_and(|kind| {
            !kind.is_empty() && kind.chars().all(|c| c.is_alphanumeric() || c == '_')
        })
}

/// `[text](target)` and `![alt](src)` as their text.
fn unlink(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let label_end = rest[open + 1..].find(']').map(|i| open + 1 + i);
        let link = label_end.and_then(|close| {
            let after = &rest[close + 1..];
            let target = after.strip_prefix('(')?.find(')')?;
            (close > open + 1).then_some((close, close + 1 + 1 + target + 1))
        });
        match link {
            Some((close, end)) => {
                let before = &rest[..open];
                out.push_str(before.strip_suffix('!').unwrap_or(before));
                out.push_str(&rest[open + 1..close]);
                rest = &rest[end..];
            }
            None => {
                out.push_str(&rest[..open + 1]);
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{bot, comment, commit, review, thread, thread_reply};
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::{actor, hours_ago};

    fn ids(items: &[ConversationItem]) -> Vec<&str> {
        items.iter().map(|i| i.id()).collect()
    }

    #[test]
    fn marker_only_bodies_are_empty() {
        assert_eq!(visible_body("  <!-- bot:state -->\n "), None);
        assert_eq!(visible_body(""), None);
        assert_eq!(
            visible_body("<!-- a --> Looks good <!-- b -->\n"),
            Some("<!-- a --> Looks good <!-- b -->")
        );
        assert_eq!(visible_body("<!-- open"), Some("<!-- open"));
    }

    #[test]
    fn threads_sit_at_their_first_comments_time() {
        let comments = vec![
            comment("c1", "ann", hours_ago(10)),
            // The thread's root, also in the flat list.
            comment("t1-root", "bob", hours_ago(8)),
            comment("c2", "ann", hours_ago(6)),
            comment("t1-reply", "ann", hours_ago(5)),
        ];
        let threads = vec![
            thread(
                "t1",
                &[("t1-reply", hours_ago(5)), ("t1-root", hours_ago(8))],
            ),
            thread("t2", &[("t2-root", hours_ago(1))]),
        ];
        let items = conversation_items(&comments, &threads);
        assert_eq!(ids(&items), ["c1", "t1", "c2", "t2"]);
        let ConversationItem::Thread(t1) = &items[1] else {
            panic!("a thread")
        };
        assert_eq!(t1.root.id, "t1-root");
        assert_eq!(t1.replies.len(), 1);
        // The conversation's copy is kept.
        assert_eq!(t1.root.body, "t1-root by bob");
        let ConversationItem::Thread(t2) = &items[3] else {
            panic!("a thread")
        };
        // Missing from the flat list: made from the thread's own copy.
        assert_eq!(t2.root.kind, CommentKind::ReviewComment);
        assert_eq!(t2.root.path.as_deref(), Some("src/lib.rs"));
    }

    #[test]
    fn a_thread_tells_what_was_not_read() {
        let mut t = thread("t", &[("a", hours_ago(3)), ("b", hours_ago(2))]);
        t.comment_count = 5;
        let item = thread_item(&t, &HashMap::new()).unwrap();
        assert_eq!(item.more_replies, 3);
        assert!(item.has_more_replies);
        assert_eq!(item.url.as_deref(), Some("https://github.com/c/a"));

        let mut t = thread("t", &[("a", hours_ago(3))]);
        t.next_comments_cursor = Some("x".into());
        let item = thread_item(&t, &HashMap::new()).unwrap();
        assert_eq!(item.more_replies, 0);
        assert!(item.has_more_replies);

        let empty = thread("t", &[]);
        assert!(thread_item(&empty, &HashMap::new()).is_none());
    }

    #[test]
    fn the_filter_hides_bots_and_settled_items_by_default() {
        let mut resolved = thread("resolved", &[("r", hours_ago(9))]);
        resolved.is_resolved = true;
        let mut bot_finding = thread("bot-thread", &[("bf", hours_ago(7))]);
        bot_finding.comments[0].author = Some(bot("coderabbit"));
        let mut answered = thread("answered", &[("bq", hours_ago(6)), ("hr", hours_ago(5))]);
        answered.comments[0].author = Some(bot("coderabbit"));
        let comments = vec![
            comment("person", "ann", hours_ago(10)),
            {
                let mut c = comment("bot-chatter", "x", hours_ago(8));
                c.author = Some(bot("ci-bot"));
                c
            },
            {
                let mut c = review("bot-approval", "x", ReviewState::Approved, hours_ago(4));
                c.author = Some(bot("ci-bot"));
                c
            },
            review("dismissed", "bob", ReviewState::Dismissed, hours_ago(3)),
        ];
        let items = conversation_items(&comments, &[resolved, bot_finding, answered]);
        let shown = |filter| {
            filter_items(&items, filter)
                .into_iter()
                .map(|i| items[i].id())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            shown(CommentFilter::default()),
            ["person", "answered", "bot-approval"]
        );
        assert_eq!(
            shown(CommentFilter {
                bots: true,
                resolved: false
            }),
            [
                "person",
                "bot-chatter",
                "bot-thread",
                "answered",
                "bot-approval"
            ]
        );
        assert_eq!(
            shown(CommentFilter {
                bots: false,
                resolved: true
            }),
            [
                "person",
                "resolved",
                "answered",
                "bot-approval",
                "dismissed"
            ]
        );
    }

    #[test]
    fn paging_shows_the_newest() {
        assert_eq!(page(25, 10), (15..25, 15));
        assert_eq!(page(4, 10), (0..4, 0));
        assert_eq!(page(0, 10), (0..0, 0));
    }

    #[test]
    fn comment_counts_include_replies() {
        let items = conversation_items(
            &[comment("c", "ann", hours_ago(3))],
            &[thread("t", &[("a", hours_ago(2)), ("b", hours_ago(1))])],
        );
        assert_eq!(comment_count(&items), 3);
    }

    #[test]
    fn each_reviewer_keeps_their_latest_verdict() {
        let comments = vec![
            review("1", "Ann", ReviewState::ChangesRequested, hours_ago(9)),
            review("2", "bob", ReviewState::Approved, hours_ago(8)),
            review("3", "ann", ReviewState::Approved, hours_ago(2)),
            // Out of order: older than ann's approval.
            review("4", "ann", ReviewState::ChangesRequested, hours_ago(5)),
            review("5", "cid", ReviewState::Approved, hours_ago(7)),
            review("6", "cid", ReviewState::Dismissed, hours_ago(6)),
            review("7", "dan", ReviewState::Commented, hours_ago(1)),
        ];
        let outcomes = latest_review_outcomes(&comments, &[commit("abc", hours_ago(4))]);
        let summary: Vec<_> = outcomes
            .iter()
            .map(|o| (o.actor.as_ref().unwrap().login.as_str(), o.outcome, o.stale))
            .collect();
        assert_eq!(
            summary,
            [
                ("ann", ReviewOutcome::Approved, false),
                ("bob", ReviewOutcome::Approved, true),
            ]
        );
    }

    #[test]
    fn reviewers_are_the_requested_then_those_who_answered() {
        let outcomes = latest_review_outcomes(
            &[
                review("1", "BOB", ReviewState::Approved, hours_ago(3)),
                review("2", "cid", ReviewState::ChangesRequested, hours_ago(2)),
            ],
            &[],
        );
        let entries = reviewer_entries(&[actor("ann"), actor("bob")], &outcomes);
        let summary: Vec<_> = entries
            .iter()
            .map(|e| (e.actor.as_ref().unwrap().login.as_str(), e.outcome))
            .collect();
        assert_eq!(
            summary,
            [
                ("ann", None),
                ("bob", Some(ReviewOutcome::Approved)),
                ("cid", Some(ReviewOutcome::ChangesRequested)),
            ]
        );
    }

    #[test]
    fn loaded_thread_comments_append_once() {
        let base = vec![
            thread_reply("a", hours_ago(3)),
            thread_reply("b", hours_ago(2)),
        ];
        let loaded = vec![
            thread_reply("b", hours_ago(2)),
            thread_reply("c", hours_ago(1)),
        ];
        let merged: Vec<_> = merge_thread_comments(&base, &loaded)
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(merged, ["a", "b", "c"]);
    }

    #[test]
    fn the_viewer_edits_their_own_comments_but_no_review_summary() {
        let mine = comment("c", "Ann", hours_ago(1));
        assert!(can_edit(&mine, Some("ann")));
        assert!(!can_edit(&mine, Some("bob")));
        assert!(!can_edit(&mine, None));
        let summary = review("r", "ann", ReviewState::Approved, hours_ago(1));
        assert!(!can_edit(&summary, Some("ann")));
    }

    #[test]
    fn splices_cover_what_changed() {
        assert_eq!(list_splice(&[1, 2, 3], &[1, 2, 3]), None);
        assert_eq!(list_splice(&[1, 2, 3], &[1, 9, 3]), Some((1..2, 1)));
        assert_eq!(list_splice(&[1, 2, 3], &[1, 2, 3, 4]), Some((3..3, 1)));
        assert_eq!(list_splice(&[1, 2, 3], &[0, 1, 2, 3]), Some((0..0, 1)));
        assert_eq!(list_splice(&[1, 2, 3], &[1, 3]), Some((1..2, 0)));
        assert_eq!(list_splice(&[1, 1], &[1]), Some((1..2, 0)));
        assert_eq!(list_splice::<u8>(&[], &[]), None);
        assert_eq!(list_splice(&[1, 2], &[3]), Some((0..2, 1)));
    }

    #[test]
    fn a_list_at_its_top_stays_there_when_rows_come_in_front() {
        let list = ListState::new(0, gpui_kit::ListAlignment::Top, px(100.));
        // As after a frame drawn with no rows.
        list.scroll_to(ListOffset::default());
        splice_list(&list, &[], &[1, 2, 3]);
        assert_eq!(list.logical_scroll_top().item_ix, 0);
        splice_list(&list, &[1, 2, 3], &[0, 1, 2, 3]);
        assert_eq!(list.logical_scroll_top().item_ix, 0);
        // Scrolled down, the row seen stays in view.
        list.scroll_to(ListOffset {
            item_ix: 2,
            offset_in_item: px(0.),
        });
        splice_list(&list, &[0, 1, 2, 3], &[9, 0, 1, 2, 3]);
        assert_eq!(list.logical_scroll_top().item_ix, 3);
    }

    #[test]
    fn authors_and_mergers_edit_the_description() {
        use crate::main_screen::right_panel::pull_request::fixtures::detail_with;
        use github::model::PullRequestAction;
        let detail = detail_with(290, |d| d.viewer_permissions.actions.clear());
        assert!(can_edit_description(&detail, Some("Daniel-Kurzynski")));
        assert!(!can_edit_description(&detail, Some("ann")));
        assert!(!can_edit_description(&detail, None));
        let merger = detail_with(290, |d| {
            d.viewer_permissions.actions = vec![PullRequestAction::Merge]
        });
        assert!(can_edit_description(&merger, Some("ann")));
    }

    #[test]
    fn previews_are_plain_text() {
        assert_eq!(
            preview_text(
                "<!-- marker -->\n> [!WARNING]\n> **Potential issue** in `parse`\n\n\
                 - See [the docs](https://x.io) and ![img](a.png)\n## Fix\n"
            ),
            "Potential issue in parse See the docs and img Fix"
        );
        assert_eq!(preview_text("[WIP] not a link"), "[WIP] not a link");
    }
}
