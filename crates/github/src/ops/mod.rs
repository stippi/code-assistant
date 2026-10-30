//! The operations, as methods of `Client`, one module per area.

mod actions;
mod activity;
mod candidates;
pub(crate) mod checks;
mod detail;
mod diff;
mod files;
mod list;
mod write;

use crate::client::Client;
use crate::convert::activity::RawSubjectRepository;
use crate::convert::{decode, trimmed};
use crate::error::{Error, Result};
use crate::{PullRequestRef, RepoRef};
use serde::Deserialize;
use serde_json::{Value, json};

/// At most this many pages of one connection are walked.
const MAX_PAGES: usize = 10;

pub(crate) fn ensure_addressable(repo: &RepoRef) -> Result<()> {
    if repo.is_addressable() {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "GitHub cannot address the repository {repo}"
        )))
    }
}

/// `owner`, `name` and `number`: what most operations start from.
pub(crate) fn pr_variables(pr: &PullRequestRef) -> Result<Value> {
    ensure_addressable(&pr.repo)?;
    if pr.number == 0 || pr.number > i32::MAX as u64 {
        return Err(Error::Invalid(format!(
            "{} is not a pull request number",
            pr.number
        )));
    }
    Ok(json!({ "owner": pr.repo.owner, "name": pr.repo.name, "number": pr.number }))
}

/// Adds variables to an object of variables.
pub(crate) fn with(mut variables: Value, extra: Value) -> Value {
    if let (Value::Object(variables), Value::Object(extra)) = (&mut variables, extra) {
        variables.extend(extra);
    }
    variables
}

/// A commit sha as GitHub accepts it in a path.
pub(crate) fn is_commit_sha(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Client {
    /// The pull request's node id, which mutations address it by.
    pub(crate) async fn node_id(&self, pr: &PullRequestRef) -> Result<String> {
        let key = format!("{} {}", pr.repo, pr.number);
        if let Some(id) = self.inner.node_ids.get(&key) {
            return Ok(id);
        }
        #[derive(Deserialize)]
        struct Data {
            repository: Option<RawSubjectRepository>,
        }
        let data = self
            .graphql_reserved(&pr.repo.host, "PullRequestNodeId", pr_variables(pr)?)
            .await?;
        let data: Data = decode("PullRequestNodeId", data)?;
        let id = data
            .repository
            .and_then(|r| r.pull_request)
            .and_then(|pr| trimmed(pr.id.as_deref()))
            .ok_or(Error::NotFound {
                operation: "PullRequestNodeId",
            })?;
        self.inner.node_ids.put(key, id.clone());
        Ok(id)
    }

    /// Mutations follow node ids; one named for another pull request is
    /// refused before it is written to.
    pub(crate) async fn ensure_subject_of(
        &self,
        pr: &PullRequestRef,
        subject_id: &str,
    ) -> Result<()> {
        #[derive(Deserialize, Default)]
        #[serde(rename_all = "camelCase", default)]
        struct Id {
            id: Option<String>,
        }
        #[derive(Deserialize, Default)]
        #[serde(rename_all = "camelCase", default)]
        struct Node {
            #[serde(rename = "__typename")]
            typename: Option<String>,
            id: Option<String>,
            pull_request: Option<Id>,
            review_comment_pull_request: Option<Id>,
            review_pull_request: Option<Id>,
        }
        #[derive(Deserialize)]
        struct Data {
            repository: Option<RawSubjectRepository>,
            node: Option<Node>,
        }
        let variables = with(pr_variables(pr)?, json!({ "subjectId": subject_id }));
        let data = self
            .graphql_reserved(&pr.repo.host, "PullRequestSubject", variables)
            .await?;
        let data: Data = decode("PullRequestSubject", data)?;
        let expected = data
            .repository
            .and_then(|r| r.pull_request)
            .and_then(|pr| pr.id);
        let actual = data.node.and_then(|node| match node.typename.as_deref() {
            Some("IssueComment") => node.pull_request.and_then(|p| p.id),
            Some("PullRequestReviewComment") => node.review_comment_pull_request.and_then(|p| p.id),
            Some("PullRequestReview") => node.review_pull_request.and_then(|p| p.id),
            _ => node.id,
        });
        if expected.is_some() && expected == actual {
            Ok(())
        } else {
            Err(Error::SubjectScope)
        }
    }
}
