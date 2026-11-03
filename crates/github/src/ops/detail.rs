//! The full read of one pull request.

use super::{MAX_PAGES, pr_variables, with};
use crate::PullRequestRef;
use crate::client::Client;
use crate::convert::checks::{RawCheck, checks, checks_state};
use crate::convert::decode;
use crate::convert::detail::{contexts_page, core};
use crate::error::{Error, PathSegment, Result};
use crate::model::Detail;
use serde_json::json;

/// GitHub refuses a requested team the credential cannot see (no
/// `read:org`) with FORBIDDEN on that one field, failing the whole read.
pub(crate) fn is_refused_requested_reviewers_only(error: &Error) -> bool {
    let Error::GraphQl { errors, .. } = error else {
        return false;
    };
    !errors.is_empty()
        && errors.iter().all(|e| {
            let field = |i: usize, name: &str| {
                matches!(e.path.get(i), Some(PathSegment::Field(f)) if f == name)
            };
            e.is_kind("FORBIDDEN")
                && e.path.len() == 6
                && field(0, "repository")
                && field(1, "pullRequest")
                && field(2, "reviewRequests")
                && field(3, "nodes")
                && matches!(e.path.get(4), Some(PathSegment::Index(_)))
                && field(5, "requestedReviewer")
        })
}

/// Every check context of the head, for a rollup past one page.
struct AllContexts {
    head_sha: Option<String>,
    contexts: Vec<RawCheck>,
    truncated: bool,
}

/// Older Enterprise servers may not know `isRequired`.
fn asks_required(host: &str) -> bool {
    host == "github.com"
}

impl Client {
    /// Everything about one pull request: state, people, labels, checks
    /// (with the workflow runs of a fork waiting for approval), merge
    /// settings and what the viewer may do.
    pub async fn detail(&self, pr: &PullRequestRef) -> Result<Detail> {
        let mut detail = self.detail_without_approvals(pr).await?;
        self.add_workflow_approvals(pr, &mut detail).await?;
        Ok(detail)
    }

    pub(crate) async fn detail_without_approvals(&self, pr: &PullRequestRef) -> Result<Detail> {
        let host = &pr.repo.host;
        let variables = |requested_reviewers: u32| -> Result<serde_json::Value> {
            Ok(with(
                pr_variables(pr)?,
                json!({
                    "headRef": format!("refs/pull/{}/head", pr.number),
                    "required": asks_required(host),
                    "requestedReviewers": requested_reviewers,
                }),
            ))
        };
        let data = match self.graphql(host, "PullRequestCore", variables(100)?).await {
            Err(e) if is_refused_requested_reviewers_only(&e) => {
                self.graphql(host, "PullRequestCore", variables(0)?).await?
            }
            answer => answer?,
        };
        let (mut detail, more_checks) =
            core(decode("PullRequestCore", data)?, host).ok_or(Error::NotFound {
                operation: "PullRequestCore",
            })?;
        // Past the first hundred checks the rest could hide a failure, so
        // the whole rollup is read.
        if more_checks {
            let all = self.all_check_contexts(pr).await?;
            if all.head_sha != detail.head_sha {
                return Err(Error::decode(
                    "PullRequestCheckContexts",
                    "the head changed while reading checks",
                ));
            }
            detail.checks = checks(&all.contexts);
            detail.checks_state = checks_state(&all.contexts);
            detail.checks_truncated = all.truncated;
        }
        Ok(detail)
    }

    async fn all_check_contexts(&self, pr: &PullRequestRef) -> Result<AllContexts> {
        let host = &pr.repo.host;
        let mut head_sha: Option<Option<String>> = None;
        let mut contexts = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let variables = with(
                pr_variables(pr)?,
                json!({ "required": asks_required(host), "after": after }),
            );
            let data = self
                .graphql(host, "PullRequestCheckContexts", variables)
                .await?;
            let page = contexts_page(decode("PullRequestCheckContexts", data)?).ok_or(
                Error::NotFound {
                    operation: "PullRequestCheckContexts",
                },
            )?;
            // A head that moves between pages would mix two revisions' checks.
            match &head_sha {
                Some(first) if *first != page.head_sha => {
                    return Err(Error::decode(
                        "PullRequestCheckContexts",
                        "the head changed while reading checks",
                    ));
                }
                Some(_) => {}
                None => head_sha = Some(page.head_sha.clone()),
            }
            contexts.extend(page.contexts);
            match page.next_cursor {
                Some(next) if Some(&next) != after.as_ref() => after = Some(next),
                Some(_) => {
                    return Ok(AllContexts {
                        head_sha: head_sha.flatten(),
                        contexts,
                        truncated: true,
                    });
                }
                None => {
                    return Ok(AllContexts {
                        head_sha: head_sha.flatten(),
                        contexts,
                        truncated: false,
                    });
                }
            }
        }
        Ok(AllContexts {
            head_sha: head_sha.flatten(),
            contexts,
            truncated: true,
        })
    }
}
