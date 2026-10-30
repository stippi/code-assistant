//! The conversation of a pull request.

use super::detail::is_refused_requested_reviewers_only;
use super::{MAX_PAGES, pr_variables, with};
use crate::PullRequestRef;
use crate::client::Client;
use crate::convert::activity::{
    ActivityRead, ThreadsRead, activity_page, assemble, commit_stats, dismissals_page,
    thread_comments_page, threads_page,
};
use crate::convert::decode;
use crate::error::{Error, Result};
use crate::model::{Activity, ThreadCommentsPage};
use serde_json::json;

impl Client {
    /// Comments, reviews, review threads, commits and reactions. The thread
    /// read failing leaves the threads out (marked truncated) rather than
    /// failing the conversation.
    pub async fn activity(&self, pr: &PullRequestRef) -> Result<Activity> {
        let (activity, threads) = futures::join!(self.read_activity(pr), self.read_threads(pr));
        let activity = activity?;
        let threads = threads.unwrap_or_else(|e| {
            tracing::warn!("Reading the review threads of {pr} failed: {e}");
            ThreadsRead::unavailable()
        });
        Ok(assemble(activity, threads, &pr.repo.host))
    }

    /// Issue comments and reviews page independently; each is switched off
    /// once it has no more pages. Commit stats come from one extra read of
    /// the non-merge commits.
    async fn read_activity(&self, pr: &PullRequestRef) -> Result<ActivityRead> {
        let host = &pr.repo.host;
        let mut read = ActivityRead {
            author: None,
            comments: Vec::new(),
            commits: Vec::new(),
        };
        let mut commit_ids = Vec::new();
        let mut comments_after: Option<String> = None;
        let mut reviews_after: Option<String> = None;
        let mut with_comments = true;
        let mut with_reviews = true;
        for page in 0..MAX_PAGES {
            if !with_comments && !with_reviews {
                break;
            }
            let variables = with(
                pr_variables(pr)?,
                json!({
                    "head": page == 0,
                    "withComments": with_comments,
                    "commentsAfter": comments_after,
                    "withReviews": with_reviews,
                    "reviewsAfter": reviews_after,
                }),
            );
            let data = self.graphql(host, "PullRequestActivity", variables).await?;
            let answer =
                activity_page(decode("PullRequestActivity", data)?).ok_or(Error::NotFound {
                    operation: "PullRequestActivity",
                })?;
            if page == 0 {
                read.author = answer.author;
                read.commits = answer.commits;
                commit_ids = answer.non_merge_commit_ids;
            }
            read.comments.extend(answer.remarks);
            with_comments = with_comments && answer.next_comments_cursor.is_some();
            with_reviews = with_reviews && answer.next_reviews_cursor.is_some();
            comments_after = answer.next_comments_cursor;
            reviews_after = answer.next_reviews_cursor;
        }
        read.comments.sort_by_key(|c| c.created_at);

        // Stats are a nicety: a failed read leaves them out.
        if !commit_ids.is_empty() {
            match self
                .graphql(host, "PullRequestCommitStats", json!({ "ids": commit_ids }))
                .await
                .and_then(|data| decode("PullRequestCommitStats", data))
            {
                Ok(data) => {
                    let stats = commit_stats(data);
                    for commit in &mut read.commits {
                        if let Some((additions, deletions)) = stats.get(&commit.oid) {
                            commit.additions = Some(*additions);
                            commit.deletions = Some(*deletions);
                        }
                    }
                }
                Err(e) => tracing::debug!("Reading commit stats of {pr} failed: {e}"),
            }
        }
        Ok(read)
    }

    async fn read_threads(&self, pr: &PullRequestRef) -> Result<ThreadsRead> {
        let host = &pr.repo.host;
        let mut requested_reviewers = 50;
        let mut read: Option<ThreadsRead> = None;
        let mut after: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let variables = |requested: u32| -> Result<serde_json::Value> {
                Ok(with(
                    pr_variables(pr)?,
                    json!({ "after": after, "requestedReviewers": requested }),
                ))
            };
            let data = match self
                .graphql(host, "ReviewThreads", variables(requested_reviewers)?)
                .await
            {
                Err(e) if requested_reviewers > 0 && is_refused_requested_reviewers_only(&e) => {
                    requested_reviewers = 0;
                    self.graphql(host, "ReviewThreads", variables(0)?).await?
                }
                answer => answer?,
            };
            let mut page = threads_page(decode("ReviewThreads", data)?).ok_or(Error::NotFound {
                operation: "ReviewThreads",
            })?;
            let threads = std::mem::take(&mut page.threads);
            let next = page.next_cursor.clone();
            let read = read.get_or_insert_with(|| ThreadsRead {
                dismissals_by_review_id: std::mem::take(&mut page.dismissals_by_review_id),
                first: page,
                ..ThreadsRead::default()
            });
            read.threads.extend(threads);
            read.threads_truncated = next.is_some();
            match next {
                Some(next) if Some(&next) != after.as_ref() => after = Some(next),
                _ => break,
            }
        }
        let mut read = read.expect("the loop reads at least one page");

        // Dismissal messages past the first page, which pull requests rarely have.
        let mut after = read.first.next_dismissal_cursor.clone();
        let mut seen = Vec::new();
        while let Some(cursor) = after.take() {
            if seen.contains(&cursor) || seen.len() >= MAX_PAGES {
                break;
            }
            let variables = with(pr_variables(pr)?, json!({ "after": cursor }));
            seen.push(cursor);
            let data = self.graphql(host, "ReviewDismissals", variables).await?;
            let (dismissals, next) =
                dismissals_page(decode("ReviewDismissals", data)?).ok_or(Error::NotFound {
                    operation: "ReviewDismissals",
                })?;
            read.dismissals_by_review_id.extend(dismissals);
            after = next;
        }
        Ok(read)
    }

    /// More comments of one review thread, from a thread's
    /// `next_comments_cursor`. The thread must belong to `pr`.
    pub async fn thread_comments(
        &self,
        pr: &PullRequestRef,
        thread_id: &str,
        cursor: &str,
    ) -> Result<ThreadCommentsPage> {
        let variables = with(
            pr_variables(pr)?,
            json!({ "threadId": thread_id, "after": cursor }),
        );
        let data = self
            .graphql(&pr.repo.host, "ReviewThreadComments", variables)
            .await?;
        let (belongs, page) = thread_comments_page(decode("ReviewThreadComments", data)?);
        if belongs {
            Ok(page)
        } else {
            Err(Error::SubjectScope)
        }
    }
}
