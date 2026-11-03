//! Merging, auto-merge, branch updates, draft state, closing and deleting
//! the source branch.

use super::pr_variables;
use crate::PullRequestRef;
use crate::client::Client;
use crate::convert::detail::IMMEDIATELY_MERGEABLE;
use crate::convert::{decode, trimmed};
use crate::error::{BranchDeletionRefusal, Error, Result};
use crate::model::{Action, MergeMethod, UpdateMethod};
use serde::Deserialize;
use serde_json::json;

const NULL_OID: &str = "0000000000000000000000000000000000000000";

fn graphql_merge_method(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Merge => "MERGE",
        MergeMethod::Squash => "SQUASH",
        MergeMethod::Rebase => "REBASE",
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ActionState {
    id: String,
    head_ref_oid: String,
    is_merge_queue_enabled: Option<bool>,
    merge_state_status: Option<String>,
    base_ref: Option<BaseRef>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct BaseRef {
    compare: Option<Compare>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Compare {
    behind_by: Option<u64>,
}

/// Whether merging goes through auto-merge: always with a merge queue;
/// for "enable auto-merge" unless the pull request can merge right now
/// (then it merges at once, as `gh pr merge --auto` does).
fn merges_automatically(action: Action, state: &ActionState) -> bool {
    let immediately = state
        .merge_state_status
        .as_deref()
        .is_some_and(|s| IMMEDIATELY_MERGEABLE.contains(&s.trim().to_ascii_uppercase().as_str()));
    state.is_merge_queue_enabled == Some(true)
        || (matches!(action, Action::EnableAutoMerge(_)) && !immediately)
}

impl Client {
    /// Runs an action on the pull request.
    pub async fn run_action(&self, pr: &PullRequestRef, action: Action) -> Result<()> {
        let simple = match action {
            Action::DisableAutoMerge => Some("DisablePullRequestAutoMerge"),
            Action::Ready => Some("MarkPullRequestReadyForReview"),
            Action::Draft => Some("ConvertPullRequestToDraft"),
            Action::Close => Some("ClosePullRequest"),
            Action::Reopen => Some("ReopenPullRequest"),
            _ => None,
        };
        if let Some(operation) = simple {
            let id = self.node_id(pr).await?;
            self.graphql(&pr.repo.host, operation, json!({ "pullRequestId": id }))
                .await?;
            return Ok(());
        }
        match action {
            Action::DeleteBranch => self.delete_branch(pr).await,
            Action::ApproveWorkflows => self.approve_workflows(pr).await,
            Action::UpdateBranch(method) => self.update_branch(pr, method).await,
            Action::Merge(method) | Action::EnableAutoMerge(method) => {
                let state = self.action_state(pr).await?;
                let input = json!({
                    "pullRequestId": state.id,
                    "mergeMethod": graphql_merge_method(method),
                });
                let operation = if merges_automatically(action, &state) {
                    "EnablePullRequestAutoMerge"
                } else {
                    "MergePullRequest"
                };
                self.graphql(&pr.repo.host, operation, json!({ "input": input }))
                    .await?;
                Ok(())
            }
            _ => unreachable!("simple actions return above"),
        }
    }

    /// The fresh head and merge-queue state an action decides by.
    async fn action_state(&self, pr: &PullRequestRef) -> Result<ActionState> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Repository {
            pull_request: Option<ActionState>,
        }
        #[derive(Deserialize)]
        struct Data {
            repository: Option<Repository>,
        }
        let mut variables = pr_variables(pr)?;
        variables["headRef"] = json!(format!("refs/pull/{}/head", pr.number));
        let data = self
            .graphql_reserved(&pr.repo.host, "PullRequestActionState", variables)
            .await?;
        let data: Data = decode("PullRequestActionState", data)?;
        data.repository
            .and_then(|r| r.pull_request)
            .ok_or(Error::NotFound {
                operation: "PullRequestActionState",
            })
    }

    /// Brings the base in, against the head the state read saw, so commits
    /// pushed meanwhile are not built over. Nothing when not behind.
    async fn update_branch(&self, pr: &PullRequestRef, method: UpdateMethod) -> Result<()> {
        let state = self.action_state(pr).await?;
        let behind_by = state
            .base_ref
            .as_ref()
            .and_then(|b| b.compare.as_ref())
            .and_then(|c| c.behind_by);
        if behind_by == Some(0) {
            return Ok(());
        }
        self.graphql(
            &pr.repo.host,
            "UpdatePullRequestBranch",
            json!({
                "pullRequestId": state.id,
                "expectedHeadOid": state.head_ref_oid,
                "updateMethod": match method {
                    UpdateMethod::Merge => "MERGE",
                    UpdateMethod::Rebase => "REBASE",
                },
            }),
        )
        .await?;
        Ok(())
    }

    /// Deletes the source branch of a closed or merged pull request, only
    /// while it still points at the pull request's head and is neither the
    /// base nor its repository's default branch. An already deleted branch
    /// is no error.
    async fn delete_branch(&self, pr: &PullRequestRef) -> Result<()> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Id {
            id: Option<String>,
        }
        #[derive(Deserialize, Default)]
        #[serde(rename_all = "camelCase", default)]
        struct Branch {
            state: Option<String>,
            head_ref_name: Option<String>,
            base_ref_name: Option<String>,
            head_ref_oid: Option<String>,
            head_repository: Option<Id>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Repository {
            pull_request: Option<Branch>,
        }
        #[derive(Deserialize)]
        struct BranchData {
            repository: Option<Repository>,
        }
        let refused = |reason| Err(Error::BranchDeletionRefused(reason));
        let host = &pr.repo.host;
        let data = self
            .graphql_reserved(host, "PullRequestBranchToDelete", pr_variables(pr)?)
            .await?;
        let branch = decode::<BranchData>("PullRequestBranchToDelete", data)?
            .repository
            .and_then(|r| r.pull_request)
            .ok_or(Error::NotFound {
                operation: "PullRequestBranchToDelete",
            })?;
        if branch
            .state
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case("OPEN"))
        {
            return refused(BranchDeletionRefusal::OpenPullRequest);
        }
        let (Some(repository_id), Some(head_ref_name), Some(head_ref_oid)) = (
            branch
                .head_repository
                .and_then(|r| trimmed(r.id.as_deref())),
            trimmed(branch.head_ref_name.as_deref()),
            trimmed(branch.head_ref_oid.as_deref()),
        ) else {
            return refused(BranchDeletionRefusal::SourceUnavailable);
        };
        if Some(&head_ref_name) == branch.base_ref_name.as_ref() {
            return refused(BranchDeletionRefusal::UnsafeBranch);
        }

        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Name {
            name: Option<String>,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Target {
            oid: Option<String>,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Ref {
            target: Option<Target>,
        }
        #[derive(Deserialize, Default)]
        #[serde(rename_all = "camelCase", default)]
        struct Source {
            #[serde(rename = "__typename")]
            typename: Option<String>,
            default_branch_ref: Option<Name>,
            #[serde(rename = "ref")]
            reference: Option<Ref>,
        }
        #[derive(Deserialize)]
        struct SourceData {
            node: Option<Source>,
        }
        let qualified = format!("refs/heads/{head_ref_name}");
        let data = self
            .graphql_reserved(
                host,
                "PullRequestSourceBranch",
                json!({ "repositoryId": repository_id, "ref": qualified }),
            )
            .await?;
        let Some(source) = decode::<SourceData>("PullRequestSourceBranch", data)?
            .node
            .filter(|node| node.typename.as_deref() == Some("Repository"))
        else {
            return refused(BranchDeletionRefusal::SourceUnavailable);
        };
        let Some(reference) = source.reference else {
            return Ok(());
        };
        let default_branch = source.default_branch_ref.and_then(|d| d.name);
        if default_branch.is_none_or(|default| default == head_ref_name) {
            return refused(BranchDeletionRefusal::UnsafeBranch);
        }
        if reference.target.and_then(|t| t.oid).as_deref() != Some(head_ref_oid.as_str()) {
            return refused(BranchDeletionRefusal::HeadChanged);
        }
        self.graphql(
            host,
            "DeletePullRequestBranch",
            json!({ "input": {
                "repositoryId": repository_id,
                "refUpdates": [{
                    "name": qualified,
                    "beforeOid": head_ref_oid,
                    "afterOid": NULL_OID,
                }],
            }}),
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(queue: bool, status: &str) -> ActionState {
        ActionState {
            is_merge_queue_enabled: Some(queue),
            merge_state_status: Some(status.into()),
            ..ActionState::default()
        }
    }

    #[test]
    fn decides_between_merging_and_arming_auto_merge() {
        let merge = Action::Merge(MergeMethod::Squash);
        let auto = Action::EnableAutoMerge(MergeMethod::Squash);
        assert!(!merges_automatically(merge, &state(false, "BLOCKED")));
        assert!(merges_automatically(merge, &state(true, "CLEAN")));
        assert!(merges_automatically(auto, &state(false, "BLOCKED")));
        assert!(!merges_automatically(auto, &state(false, "CLEAN")));
        assert!(!merges_automatically(auto, &state(false, "unstable")));
        assert!(merges_automatically(auto, &state(true, "HAS_HOOKS")));
    }
}
