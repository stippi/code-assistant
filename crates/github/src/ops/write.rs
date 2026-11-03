//! Comments, reviews, threads, reactions, reviewers and labels.

use super::pr_variables;
use crate::PullRequestRef;
use crate::client::{Client, RestCall};
use crate::convert::review::{review_submission_body, reviewer_request_body};
use crate::error::Result;
use crate::model::{
    EditableCommentKind, PullRequestUpdate, ReactionContent, ReviewSubmission, Reviewer,
};
use crate::transport::Method;
use serde_json::json;

impl Client {
    /// Adds a comment to the conversation.
    pub async fn add_comment(&self, pr: &PullRequestRef, body: &str) -> Result<()> {
        let subject_id = self.node_id(pr).await?;
        self.graphql(
            &pr.repo.host,
            "AddComment",
            json!({ "subjectId": subject_id, "body": body }),
        )
        .await?;
        Ok(())
    }

    /// Rewrites a comment of this pull request.
    pub async fn update_comment(
        &self,
        pr: &PullRequestRef,
        comment_id: &str,
        kind: EditableCommentKind,
        body: &str,
    ) -> Result<()> {
        self.ensure_subject_of(pr, comment_id).await?;
        let operation = match kind {
            EditableCommentKind::IssueComment => "UpdateIssueComment",
            EditableCommentKind::ReviewComment => "UpdateReviewComment",
        };
        self.graphql(
            &pr.repo.host,
            operation,
            json!({ "commentId": comment_id, "body": body }),
        )
        .await?;
        Ok(())
    }

    /// Rewrites the title, description or maintainer-edit setting; fields
    /// left `None` stay as they are.
    pub async fn update_pull_request(
        &self,
        pr: &PullRequestRef,
        update: &PullRequestUpdate,
    ) -> Result<()> {
        let mut variables = json!({ "pullRequestId": self.node_id(pr).await? });
        if let Some(title) = &update.title {
            variables["title"] = json!(title);
        }
        if let Some(body) = &update.body {
            variables["body"] = json!(body);
        }
        if let Some(allowed) = update.maintainer_can_modify {
            variables["maintainerCanModify"] = json!(allowed);
        }
        self.graphql(&pr.repo.host, "UpdatePullRequest", variables)
            .await?;
        Ok(())
    }

    /// Submits a review with its line comments in one request.
    pub async fn submit_review(
        &self,
        pr: &PullRequestRef,
        review: &ReviewSubmission,
    ) -> Result<()> {
        pr_variables(pr)?;
        let repo = &pr.repo;
        self.rest(
            RestCall::new(
                &repo.host,
                "submitReview",
                Method::Post,
                format!(
                    "repos/{}/{}/pulls/{}/reviews",
                    repo.owner, repo.name, pr.number
                ),
            )
            .json(review_submission_body(review)),
        )
        .await?;
        Ok(())
    }

    /// Replies in a review thread.
    pub async fn reply_to_thread(
        &self,
        pr: &PullRequestRef,
        thread_id: &str,
        body: &str,
    ) -> Result<()> {
        self.graphql(
            &pr.repo.host,
            "AddReviewThreadReply",
            json!({ "threadId": thread_id, "body": body }),
        )
        .await?;
        Ok(())
    }

    pub async fn set_thread_resolved(
        &self,
        pr: &PullRequestRef,
        thread_id: &str,
        resolved: bool,
    ) -> Result<()> {
        let operation = if resolved {
            "ResolveReviewThread"
        } else {
            "UnresolveReviewThread"
        };
        self.graphql(&pr.repo.host, operation, json!({ "threadId": thread_id }))
            .await?;
        Ok(())
    }

    /// Adds or removes the viewer's reaction on a comment or review of this
    /// pull request (`subject_id`), or on the pull request itself (`None`).
    pub async fn set_reaction(
        &self,
        pr: &PullRequestRef,
        subject_id: Option<&str>,
        content: ReactionContent,
        reacted: bool,
    ) -> Result<()> {
        let subject_id = match subject_id {
            Some(subject_id) => {
                self.ensure_subject_of(pr, subject_id).await?;
                subject_id.to_string()
            }
            None => self.node_id(pr).await?,
        };
        let operation = if reacted {
            "AddReaction"
        } else {
            "RemoveReaction"
        };
        self.graphql(
            &pr.repo.host,
            operation,
            json!({ "subjectId": subject_id, "content": content.graphql_name() }),
        )
        .await?;
        Ok(())
    }

    /// Asks people and teams for a review, or takes the request back.
    pub async fn set_reviewers_requested(
        &self,
        pr: &PullRequestRef,
        reviewers: &[Reviewer],
        requested: bool,
    ) -> Result<()> {
        if reviewers.is_empty() {
            return Ok(());
        }
        pr_variables(pr)?;
        let repo = &pr.repo;
        self.rest(
            RestCall::new(
                &repo.host,
                "setReviewersRequested",
                if requested {
                    Method::Post
                } else {
                    Method::Delete
                },
                format!(
                    "repos/{}/{}/pulls/{}/requested_reviewers",
                    repo.owner, repo.name, pr.number
                ),
            )
            .json(reviewer_request_body(reviewers)),
        )
        .await?;
        Ok(())
    }

    /// Applies labels, or takes them off one by one (GitHub has no bulk
    /// removal).
    pub async fn set_labels(
        &self,
        pr: &PullRequestRef,
        labels: &[String],
        applied: bool,
    ) -> Result<()> {
        if labels.is_empty() {
            return Ok(());
        }
        pr_variables(pr)?;
        let repo = &pr.repo;
        let issue = format!(
            "repos/{}/{}/issues/{}/labels",
            repo.owner, repo.name, pr.number
        );
        if applied {
            self.rest(
                RestCall::new(&repo.host, "setLabels", Method::Post, issue)
                    .json(json!({ "labels": labels })),
            )
            .await?;
            return Ok(());
        }
        for label in labels {
            let path = format!("{issue}/{}", urlencoding::encode(label));
            self.rest(RestCall::new(&repo.host, "setLabels", Method::Delete, path))
                .await?;
        }
        Ok(())
    }
}
