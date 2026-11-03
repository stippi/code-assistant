//! What is written but not sent on each pull request: the comment, the text
//! of the review and the line comments added "to review" in the Code tab,
//! which wait (as dashed cards on their lines) until the review is submitted
//! with a verdict. App-wide, one [`ReviewDrafts`] entity ([`review_drafts`]);
//! views observe it. Saved shortly after each change, so they outlast the
//! page and the app.
//!
//! Submitting, for the composer:
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

use super::composer::ComposerMode;
pub use crate::shared::review_draft_store::PendingComment;
use crate::shared::review_draft_store::{PullRequestDraft, ReviewDraftStore, SavedReviewDraft};
use github::PullRequestRef;
use github::model::{ReviewSubmission, ReviewVerdict};
use gpui_kit::{App, AppContext, Context, Entity, Global, Subscription, Task};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// How long changes gather before they are written.
const SAVE_DELAY: Duration = Duration::from_millis(500);

#[derive(Default)]
pub struct ReviewDrafts {
    drafts: HashMap<PullRequestRef, PullRequestDraft>,
    last_id: u64,
    /// The form the composer of a pull request is asked to open.
    opening: Option<(PullRequestRef, ComposerMode)>,
    store: Option<Arc<dyn ReviewDraftStore>>,
    _save: Option<Task<()>>,
    _quit: Option<Subscription>,
}

struct ReviewDraftsGlobal(Entity<ReviewDrafts>);

impl Global for ReviewDraftsGlobal {}

/// The app's drafts, read from the app's store with the first use.
pub fn review_drafts(cx: &mut App) -> Entity<ReviewDrafts> {
    if let Some(global) = cx.try_global::<ReviewDraftsGlobal>() {
        return global.0.clone();
    }
    let store = cx
        .try_global::<crate::Gpui>()
        .map(|gpui| gpui.stores.review_drafts.clone());
    let drafts = cx.new(|cx| match store {
        Some(store) => ReviewDrafts::with_store(store, cx),
        None => ReviewDrafts::default(),
    });
    cx.set_global(ReviewDraftsGlobal(drafts.clone()));
    drafts
}

impl ReviewDrafts {
    /// The drafts `store` holds, saved there from now on. A store that can't
    /// be read leaves them empty; the text is not worth keeping the page from.
    pub fn with_store(store: Arc<dyn ReviewDraftStore>, cx: &mut Context<Self>) -> Self {
        let mut this = Self::default();
        match store.load() {
            Ok(saved) => {
                for SavedReviewDraft { pr, mut draft } in saved {
                    for comment in &mut draft.pending {
                        this.last_id += 1;
                        comment.id = this.last_id;
                    }
                    this.drafts.insert(pr, draft);
                }
            }
            Err(error) => tracing::warn!("Couldn't read the pull request drafts: {error:#}"),
        }
        this.store = Some(store);
        this._quit = Some(cx.on_app_quit(|this: &mut Self, _| {
            // What the delay still holds back.
            if this._save.take().is_some() {
                this.save_now();
            }
            async {}
        }));
        this
    }

    fn saved(&self) -> Vec<SavedReviewDraft> {
        self.drafts
            .iter()
            .map(|(pr, draft)| SavedReviewDraft {
                pr: pr.clone(),
                draft: draft.clone(),
            })
            .collect()
    }

    fn save_now(&self) {
        if let Some(store) = &self.store
            && let Err(error) = store.save(&self.saved())
        {
            tracing::warn!("Couldn't save the pull request drafts: {error:#}");
        }
    }

    /// Tell the views, and write it all once changes pause.
    fn changed(&mut self, pr: &PullRequestRef, cx: &mut Context<Self>) {
        if self.drafts.get(pr).is_some_and(PullRequestDraft::is_empty) {
            self.drafts.remove(pr);
        }
        cx.notify();
        if self.store.is_none() {
            return;
        }
        self._save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let write = this.read_with(cx, |this, _| (this.store.clone(), this.saved()));
            if let Ok((Some(store), saved)) = write {
                cx.background_spawn(async move {
                    if let Err(error) = store.save(&saved) {
                        tracing::warn!("Couldn't save the pull request drafts: {error:#}");
                    }
                })
                .await;
            }
        }));
    }

    /// The unsent comment on `pr`.
    pub fn comment(&self, pr: &PullRequestRef) -> &str {
        self.drafts.get(pr).map_or("", |draft| &draft.comment)
    }

    /// The unsent text of `pr`'s review.
    pub fn summary(&self, pr: &PullRequestRef) -> &str {
        self.drafts.get(pr).map_or("", |draft| &draft.summary)
    }

    pub fn set_comment(&mut self, pr: &PullRequestRef, text: &str, cx: &mut Context<Self>) {
        if self.comment(pr) != text {
            self.drafts.entry(pr.clone()).or_default().comment = text.to_owned();
            self.changed(pr, cx);
        }
    }

    pub fn set_summary(&mut self, pr: &PullRequestRef, text: &str, cx: &mut Context<Self>) {
        if self.summary(pr) != text {
            self.drafts.entry(pr.clone()).or_default().summary = text.to_owned();
            self.changed(pr, cx);
        }
    }

    /// Add `quote` to the comment on `pr` and open the composer on it.
    pub fn quote(&mut self, pr: &PullRequestRef, quote: &str, cx: &mut Context<Self>) {
        let text = match self.comment(pr).trim_end() {
            "" => quote.to_owned(),
            written => format!("{written}\n\n{quote}"),
        };
        self.set_comment(pr, &text, cx);
        self.ask_to_open(pr, ComposerMode::Comment, cx);
    }

    /// Drop the review's text and its line comments.
    pub fn discard_review(&mut self, pr: &PullRequestRef, cx: &mut Context<Self>) {
        if let Some(draft) = self.drafts.get_mut(pr) {
            draft.summary.clear();
            draft.pending.clear();
            self.changed(pr, cx);
        }
    }

    /// Ask the composer of `pr` to open on `mode`.
    pub fn ask_to_open(&mut self, pr: &PullRequestRef, mode: ComposerMode, cx: &mut Context<Self>) {
        self.opening = Some((pr.clone(), mode));
        cx.notify();
    }

    /// The form the composer of `pr` is asked to open.
    pub fn opening(&self, pr: &PullRequestRef) -> Option<ComposerMode> {
        self.opening
            .as_ref()
            .filter(|(asked, _)| asked == pr)
            .map(|&(_, mode)| mode)
    }

    pub fn opened(&mut self) {
        self.opening = None;
    }

    /// `pr`'s pending comments, in the order they were added.
    pub fn comments(&self, pr: &PullRequestRef) -> &[PendingComment] {
        self.drafts.get(pr).map_or(&[], |draft| &draft.pending)
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
        self.drafts
            .entry(pr.clone())
            .or_default()
            .pending
            .push(comment);
        self.changed(pr, cx);
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
        let Some(draft) = self.drafts.get_mut(pr) else {
            return;
        };
        let before = draft.pending.len();
        draft.pending.retain(|c| !ids.contains(&c.id));
        if draft.pending.len() != before {
            self.changed(pr, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockReviewDraftStore;
    use github::RepoRef;
    use github::model::{DiffSide, ReviewCommentDraft};
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

    #[gpui_kit::test]
    fn a_quote_goes_below_what_is_written_and_opens_the_composer(cx: &mut TestAppContext) {
        let drafts = cx.update(|cx| cx.new(|_| ReviewDrafts::default()));
        drafts.update(cx, |drafts, cx| {
            drafts.quote(&pr(1), "> one\n\n", cx);
            assert_eq!(drafts.comment(&pr(1)), "> one\n\n");
            assert_eq!(drafts.opening(&pr(1)), Some(ComposerMode::Comment));
            drafts.set_comment(&pr(1), "> one\n\nSure\n", cx);
            drafts.quote(&pr(1), "> two\n\n", cx);
            assert_eq!(drafts.comment(&pr(1)), "> one\n\nSure\n\n> two\n\n");
        });
    }

    #[gpui_kit::test]
    fn drafts_outlast_the_app_once_changes_pause(cx: &mut TestAppContext) {
        let store = MockReviewDraftStore::default();
        let shared: Arc<dyn ReviewDraftStore> = Arc::new(store.clone());
        let drafts = cx.update(|cx| cx.new(|cx| ReviewDrafts::with_store(shared.clone(), cx)));
        drafts.update(cx, |drafts, cx| {
            drafts.set_comment(&pr(1), "Almost", cx);
            drafts.set_summary(&pr(1), "Fine", cx);
            drafts.add(&pr(2), comment("a.rs", 3, "Why?"), cx);
        });
        cx.run_until_parked();
        assert_eq!(store.saved().len(), 0, "not with every key");

        cx.executor().advance_clock(SAVE_DELAY);
        cx.run_until_parked();
        assert_eq!(store.saved().len(), 2);

        let again = cx.update(|cx| cx.new(|cx| ReviewDrafts::with_store(shared.clone(), cx)));
        again.read_with(cx, |again, _| {
            assert_eq!(again.comment(&pr(1)), "Almost");
            assert_eq!(again.summary(&pr(1)), "Fine");
            assert_eq!(again.comments(&pr(2))[0].body, "Why?");
            assert_ne!(again.comments(&pr(2))[0].id, 0, "usable ids");
        });
    }

    #[gpui_kit::test]
    fn a_draft_emptied_again_is_not_kept(cx: &mut TestAppContext) {
        let store = MockReviewDraftStore::default();
        let shared: Arc<dyn ReviewDraftStore> = Arc::new(store.clone());
        let drafts = cx.update(|cx| cx.new(|cx| ReviewDrafts::with_store(shared, cx)));
        drafts.update(cx, |drafts, cx| {
            drafts.set_comment(&pr(1), "Almost", cx);
            drafts.set_comment(&pr(1), "", cx);
        });
        cx.executor().advance_clock(SAVE_DELAY);
        cx.run_until_parked();
        assert_eq!(store.saved().len(), 0);
    }
}
