//! The pending review of each pull request: line comments added "to
//! review" in the Code tab, which wait (as dashed cards on their lines)
//! until the review is submitted with a verdict and a summary. In memory
//! only, app-wide, like T3 Code's `pullRequestReviewStore`.
//!
//! One [`ReviewDrafts`] entity per app ([`review_drafts`]); views observe
//! it. Submitting, for the composer:
//!
//! ```ignore
//! let drafts = review_drafts(cx);
//! let (review, ids) = drafts.read(cx).submission(&pr, verdict, body);
//! // service.pull_request_submit_review(pr.clone(), review).await, then on success:
//! drafts.update(cx, |drafts, cx| drafts.remove_submitted(&pr, &ids, cx));
//! ```
//!
//! Only the submitted comments go: one added while the review was under
//! way stays pending.

use github::PullRequestRef;
use github::model::{DiffSide, ReviewCommentDraft, ReviewSubmission, ReviewVerdict};
use gpui_kit::{App, AppContext, Context, Entity, Global};
use std::collections::HashMap;

/// A line comment of the pending review, anchored like GitHub wants it:
/// `line` on `side` (LEFT the old file, RIGHT the new one), a range from
/// `start_line` on `start_side`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingComment {
    /// Unique in the app; given by [`ReviewDrafts::add`].
    pub id: u64,
    pub path: String,
    pub side: DiffSide,
    pub line: u64,
    pub start_side: Option<DiffSide>,
    pub start_line: Option<u64>,
    pub body: String,
    /// The commented rows as shown, `+`/`-`/` ` prefixed.
    pub excerpt: String,
}

impl PendingComment {
    pub fn draft(&self) -> ReviewCommentDraft {
        ReviewCommentDraft {
            path: self.path.clone(),
            line: self.line,
            side: self.side,
            start_line: self.start_line,
            start_side: self.start_side,
            body: self.body.clone(),
        }
    }
}

#[derive(Default)]
pub struct ReviewDrafts {
    pending: HashMap<PullRequestRef, Vec<PendingComment>>,
    last_id: u64,
}

struct ReviewDraftsGlobal(Entity<ReviewDrafts>);

impl Global for ReviewDraftsGlobal {}

/// The app's pending reviews, created with the first use.
pub fn review_drafts(cx: &mut App) -> Entity<ReviewDrafts> {
    if let Some(global) = cx.try_global::<ReviewDraftsGlobal>() {
        return global.0.clone();
    }
    let drafts = cx.new(|_| ReviewDrafts::default());
    cx.set_global(ReviewDraftsGlobal(drafts.clone()));
    drafts
}

impl ReviewDrafts {
    /// `pr`'s pending comments, in the order they were added.
    pub fn comments(&self, pr: &PullRequestRef) -> &[PendingComment] {
        self.pending.get(pr).map_or(&[], Vec::as_slice)
    }

    pub fn count(&self, pr: &PullRequestRef) -> usize {
        self.comments(pr).len()
    }

    /// Add `comment` (its `id` is replaced) to `pr`'s review; returns the id.
    pub fn add(
        &mut self,
        pr: &PullRequestRef,
        mut comment: PendingComment,
        cx: &mut Context<Self>,
    ) -> u64 {
        self.last_id += 1;
        comment.id = self.last_id;
        self.pending.entry(pr.clone()).or_default().push(comment);
        cx.notify();
        self.last_id
    }

    pub fn remove(&mut self, pr: &PullRequestRef, id: u64, cx: &mut Context<Self>) {
        self.remove_submitted(pr, &[id], cx);
    }

    /// The review to send: `verdict`, `body` and every pending comment,
    /// plus the ids of those comments for [`Self::remove_submitted`].
    pub fn submission(
        &self,
        pr: &PullRequestRef,
        verdict: ReviewVerdict,
        body: String,
    ) -> (ReviewSubmission, Vec<u64>) {
        let comments = self.comments(pr);
        (
            ReviewSubmission {
                verdict,
                body,
                comments: comments.iter().map(PendingComment::draft).collect(),
            },
            comments.iter().map(|c| c.id).collect(),
        )
    }

    /// Drop the comments a submitted review carried.
    pub fn remove_submitted(&mut self, pr: &PullRequestRef, ids: &[u64], cx: &mut Context<Self>) {
        let Some(comments) = self.pending.get_mut(pr) else {
            return;
        };
        let before = comments.len();
        comments.retain(|c| !ids.contains(&c.id));
        if comments.len() != before {
            if comments.is_empty() {
                self.pending.remove(pr);
            }
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use github::RepoRef;
    use gpui_kit::TestAppContext;

    fn pr(number: u64) -> PullRequestRef {
        RepoRef::new("github.com", "o", "r").pull_request(number)
    }

    fn comment(path: &str, line: u64, body: &str) -> PendingComment {
        PendingComment {
            id: 0,
            path: path.into(),
            side: DiffSide::Right,
            line,
            start_side: None,
            start_line: None,
            body: body.into(),
            excerpt: "+x".into(),
        }
    }

    #[gpui_kit::test]
    fn comments_are_kept_per_pull_request(cx: &mut TestAppContext) {
        let drafts = cx.update(review_drafts);
        assert_eq!(
            cx.update(review_drafts).entity_id(),
            drafts.entity_id(),
            "one per app"
        );
        drafts.update(cx, |drafts, cx| {
            let a = drafts.add(&pr(1), comment("a.rs", 3, "one"), cx);
            let b = drafts.add(&pr(1), comment("b.rs", 4, "two"), cx);
            drafts.add(&pr(2), comment("c.rs", 5, "elsewhere"), cx);
            assert_ne!(a, b);
            assert_eq!(drafts.count(&pr(1)), 2);
            drafts.remove(&pr(1), a, cx);
            let left: Vec<_> = drafts.comments(&pr(1)).iter().map(|c| c.id).collect();
            assert_eq!(left, [b]);
            assert_eq!(drafts.count(&pr(2)), 1);
            assert_eq!(drafts.count(&pr(3)), 0);
        });
    }

    #[gpui_kit::test]
    fn a_submission_carries_the_comments_and_removes_only_those(cx: &mut TestAppContext) {
        let drafts = cx.update(|cx| cx.new(|_| ReviewDrafts::default()));
        let notified = std::rc::Rc::new(std::cell::Cell::new(0));
        let _observer = cx.update(|cx| {
            let notified = notified.clone();
            cx.observe(&drafts, move |_, _| notified.set(notified.get() + 1))
        });
        drafts.update(cx, |drafts, cx| {
            let mut ranged = comment("a.rs", 9, "range");
            ranged.start_line = Some(7);
            ranged.start_side = Some(DiffSide::Left);
            drafts.add(&pr(1), ranged, cx);
            let (review, ids) =
                drafts.submission(&pr(1), ReviewVerdict::Approve, "Looks good".into());
            assert_eq!(review.verdict, ReviewVerdict::Approve);
            assert_eq!(review.body, "Looks good");
            assert_eq!(
                review.comments,
                [ReviewCommentDraft {
                    path: "a.rs".into(),
                    line: 9,
                    side: DiffSide::Right,
                    start_line: Some(7),
                    start_side: Some(DiffSide::Left),
                    body: "range".into(),
                }]
            );
            // Added while the review was under way: stays.
            let late = drafts.add(&pr(1), comment("b.rs", 1, "late"), cx);
            drafts.remove_submitted(&pr(1), &ids, cx);
            let left: Vec<_> = drafts.comments(&pr(1)).iter().map(|c| c.id).collect();
            assert_eq!(left, [late]);
        });
        cx.run_until_parked();
        assert!(notified.get() > 0);
    }
}
