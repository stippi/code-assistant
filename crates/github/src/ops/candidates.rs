//! What the reviewer and label pickers offer.

use super::pr_variables;
use crate::PullRequestRef;
use crate::client::Client;
use crate::convert::candidates::{label_candidates, reviewer_candidates};
use crate::convert::decode;
use crate::error::{Error, Result};
use crate::model::{LabelCandidates, ReviewerCandidates};

impl Client {
    /// People a review can be asked of, and everyone already asked.
    pub async fn reviewer_candidates(&self, pr: &PullRequestRef) -> Result<ReviewerCandidates> {
        let data = self
            .graphql_reserved(&pr.repo.host, "ReviewerCandidates", pr_variables(pr)?)
            .await?;
        reviewer_candidates(decode("ReviewerCandidates", data)?).ok_or(Error::NotFound {
            operation: "ReviewerCandidates",
        })
    }

    /// The repository's labels, marked where applied.
    pub async fn label_candidates(&self, pr: &PullRequestRef) -> Result<LabelCandidates> {
        let data = self
            .graphql_reserved(&pr.repo.host, "LabelCandidates", pr_variables(pr)?)
            .await?;
        label_candidates(decode("LabelCandidates", data)?).ok_or(Error::NotFound {
            operation: "LabelCandidates",
        })
    }
}
