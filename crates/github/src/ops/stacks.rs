//! GitHub's stacked pull requests, ported from T3 Code: the stack a pull
//! request is a layer of (`repos/o/n/stacks?pull_request=N`, revalidated
//! with ETags), where listed pull requests sit in theirs (github.com's
//! GraphQL `stack` / `stackEntry`), merging a stack through one layer
//! (`merge-async`, polled until GitHub settles) and what rebasing a stack
//! needs. GitHub has no API to rebase a stack: the caller replays each
//! layer onto the one below in a scratch clone and pushes it
//! ([`StackRebase`]).

use super::pr_variables;
use crate::PullRequestRef;
use crate::client::{Client, RestCall};
use crate::convert::detail::can_write;
use crate::error::{Error, Result};
use crate::model::{
    ListEntry, MergeMethod, PullRequestState, RebaseLayer, Stack, StackHead, StackLayer,
    StackMembership, StackRebase,
};
use crate::transport::Method;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use tokio::time::Instant;

/// How long a stack merge is polled before it is left to GitHub.
const MERGE_DEADLINE: Duration = Duration::from_secs(5 * 60);
const MERGE_POLL_MAX: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
#[serde(untagged)]
enum RawId {
    Number(u64),
    Text(String),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawBase {
    Name(String),
    Ref { r#ref: String },
}

#[derive(Deserialize)]
struct RawHead {
    r#ref: String,
    #[serde(default)]
    sha: Option<String>,
}

#[derive(Deserialize)]
struct RawLayer {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    draft: Option<bool>,
    number: u64,
    head: RawHead,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    merged_at: Option<String>,
}

#[derive(Deserialize)]
struct RawStack {
    #[serde(default)]
    id: Option<RawId>,
    number: u64,
    #[serde(default)]
    node_id: Option<String>,
    url: String,
    #[serde(default)]
    html_url: Option<String>,
    base: RawBase,
    pull_requests: Vec<RawLayer>,
}

fn trimmed(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

fn layer_state(state: Option<&str>, merged_at: Option<&str>) -> PullRequestState {
    let state = state.map(str::to_ascii_uppercase);
    if merged_at.is_some_and(|at| !at.trim().is_empty()) || state.as_deref() == Some("MERGED") {
        PullRequestState::Merged
    } else if state.as_deref() == Some("CLOSED") {
        PullRequestState::Closed
    } else {
        PullRequestState::Open
    }
}

/// The first stack of a listing (`None` for an empty one).
pub(crate) fn decode_stacks(operation: &'static str, body: &[u8]) -> Result<Option<Stack>> {
    let stacks: Vec<RawStack> =
        serde_json::from_slice(body).map_err(|e| Error::decode(operation, e))?;
    Ok(stacks.into_iter().next().map(|stack| Stack {
        id: match stack.id {
            Some(RawId::Number(id)) => id.to_string(),
            Some(RawId::Text(id)) => id,
            None => trimmed(stack.node_id).unwrap_or_else(|| stack.number.to_string()),
        },
        number: stack.number,
        url: trimmed(stack.html_url).unwrap_or(stack.url),
        base: match stack.base {
            RawBase::Name(name) => name,
            RawBase::Ref { r#ref } => r#ref,
        },
        layers: stack
            .pull_requests
            .into_iter()
            .map(|layer| StackLayer {
                number: layer.number,
                title: layer.title,
                is_draft: layer.draft,
                head_sha: layer.head.sha,
                head_branch: layer.head.r#ref,
                state: layer_state(layer.state.as_deref(), layer.merged_at.as_deref()),
            })
            .collect(),
    }))
}

/// T3 Code's refusals, in its words.
fn changed(number: u64, completed: usize) -> Error {
    Error::Refused(match completed {
        0 => "The stack changed. Refresh it before trying again.".into(),
        _ => format!(
            "The stack changed at PR #{number} after {completed} layers. Earlier updates remain \
             on GitHub. Refresh it before trying again."
        ),
    })
}

fn unsupported() -> Error {
    Error::Refused("This operation is not supported for this stack.".into())
}

/// The layers a stack action works on, checked against what the viewer
/// saw: the stack must still be `stack_number`, hold `number` (the top
/// for a rebase), and its open layers must have the expected heads.
fn affected_layers(
    stack: Option<Stack>,
    number: u64,
    stack_number: u64,
    expected: &[StackHead],
    merge: bool,
) -> Result<(Stack, Vec<StackLayer>)> {
    let stack = stack
        .filter(|stack| stack.number == stack_number)
        .ok_or_else(|| changed(number, 0))?;
    let target = stack
        .layers
        .iter()
        .position(|layer| layer.number == number)
        .filter(|&ix| merge || ix + 1 == stack.layers.len())
        .ok_or_else(|| changed(number, 0))?;
    if merge && stack.layers[target].state != PullRequestState::Open {
        return Err(unsupported());
    }
    let end = if merge {
        target + 1
    } else {
        stack.layers.len()
    };
    let open: Vec<StackLayer> = stack.layers[..end]
        .iter()
        .filter(|layer| layer.state != PullRequestState::Merged)
        .cloned()
        .collect();
    let mut numbers: Vec<u64> = expected.iter().map(|head| head.number).collect();
    numbers.sort_unstable();
    numbers.dedup();
    let matches = expected.len() == open.len()
        && numbers.len() == open.len()
        && open.iter().all(|layer| {
            layer.head_sha.as_ref().is_some_and(|sha| {
                expected
                    .iter()
                    .any(|head| head.number == layer.number && &head.head_sha == sha)
            })
        });
    if !matches {
        return Err(changed(number, 0));
    }
    if open.is_empty() || open.iter().any(|l| l.state != PullRequestState::Open) {
        return Err(unsupported());
    }
    Ok((stack, open))
}

impl Client {
    /// The stack `pr` is a layer of, with each layer's title, draft state
    /// and head (`None` when it is in none, or the host has no stacks).
    pub async fn stack(&self, pr: &PullRequestRef) -> Result<Option<Stack>> {
        pr_variables(pr)?;
        let root = format!("repos/{}/{}/stacks", pr.repo.owner, pr.repo.name);
        let found = self
            .rest_revalidated(RestCall::get(
                &pr.repo.host,
                "stack",
                format!("{root}?pull_request={}", pr.number),
            ))
            .await;
        let listed = match found {
            Ok(answer) => decode_stacks("stack", &answer.body)?,
            Err(Error::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        let Some(listed) = listed else {
            return Ok(None);
        };
        let detail = self
            .rest(RestCall::get(
                &pr.repo.host,
                "stack",
                format!("{root}/{}", listed.number),
            ))
            .await;
        match detail {
            Ok(response) => {
                let mut body = b"[".to_vec();
                body.extend(&response.body);
                body.push(b']');
                decode_stacks("stack", &body)
            }
            Err(Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The stacks the entries of a github.com listing sit in, read
    /// aliased 25 to a request. Failing only leaves them unmarked.
    pub(crate) async fn mark_stacks(&self, repo: &crate::RepoRef, entries: &mut [ListEntry]) {
        if repo.host != "github.com" || entries.is_empty() {
            return;
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RawMembershipStack {
            number: u64,
            size: u64,
            base_ref_name: String,
        }
        #[derive(Deserialize)]
        struct RawEntry {
            position: u64,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RawMembership {
            stack: Option<RawMembershipStack>,
            stack_entry: Option<RawEntry>,
        }
        let prs: Vec<PullRequestRef> = entries
            .iter()
            .map(|entry| repo.pull_request(entry.number))
            .collect();
        let answers = match self.batched(&prs, "PullRequestStackMembership").await {
            Ok(answers) => answers,
            Err(e) => {
                tracing::debug!("Pull request stack membership enrichment failed: {e}");
                return;
            }
        };
        for (entry, answer) in entries.iter_mut().zip(answers) {
            let membership = answer.ok().and_then(|data| {
                let raw = data.pointer("/repository/pullRequest")?.clone();
                let raw: RawMembership = serde_json::from_value(raw).ok()?;
                let (stack, at) = (raw.stack?, raw.stack_entry?);
                Some(StackMembership {
                    number: stack.number,
                    position: at.position,
                    size: stack.size,
                    base: stack.base_ref_name,
                })
            });
            entry.stack = membership;
        }
    }

    /// Merge the stack through `pr`: GitHub merges its unmerged layers
    /// below too (or queues them) and rebases the rest. Refused when the
    /// stack changed since `expected` was seen, or a layer to merge is a
    /// draft.
    pub async fn merge_stack(
        &self,
        pr: &PullRequestRef,
        stack_number: u64,
        expected: &[StackHead],
        method: MergeMethod,
    ) -> Result<()> {
        let stack = self.fresh_stack(pr).await?;
        let (stack, open) = affected_layers(stack, pr.number, stack_number, expected, true)?;
        if open.iter().any(|layer| layer.is_draft == Some(true)) {
            return Err(unsupported());
        }
        let target = stack
            .layers
            .iter()
            .find(|layer| layer.number == pr.number)
            .and_then(|layer| layer.head_sha.clone());
        let root = format!(
            "repos/{}/{}/pulls/{}",
            pr.repo.owner, pr.repo.name, pr.number
        );
        let method = match method {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        };
        let answer = self
            .rest(
                RestCall::new(
                    &pr.repo.host,
                    "mergeStack",
                    Method::Put,
                    format!("{root}/merge-async"),
                )
                .json(json!({ "merge_method": method, "merge_action": "default", "sha": target })),
            )
            .await?;
        #[derive(Deserialize)]
        struct Details {
            uuid: Option<String>,
        }
        #[derive(Deserialize)]
        struct Merge {
            status: String,
            details: Details,
        }
        let invalid =
            || Error::Refused("GitHub returned an unreadable stack operation response.".into());
        let decode = |body: &[u8]| serde_json::from_slice::<Merge>(body).map_err(|_| invalid());
        let mut merge = decode(&answer.body)?;
        let deadline = Instant::now() + MERGE_DEADLINE;
        let mut attempt = 0;
        while merge.status == "pending" && Instant::now() < deadline {
            let uuid = merge.details.uuid.clone().ok_or_else(invalid)?;
            let wait = Duration::from_millis(1000u64.saturating_mul(1 << attempt.min(10)));
            tokio::time::sleep(wait.min(MERGE_POLL_MAX)).await;
            attempt += 1;
            let poll = self
                .rest(RestCall::get(
                    &pr.repo.host,
                    "mergeStack",
                    format!("{root}/merge-async/{}", urlencoding::encode(&uuid)),
                ))
                .await?;
            merge = decode(&poll.body)?;
        }
        match merge.status.as_str() {
            "merged" | "enqueued" => Ok(()),
            "pending" => Err(Error::Refused(
                "The merge is still running on GitHub. Check its status there before submitting \
                 another request."
                    .into(),
            )),
            "failed" => Err(Error::Refused(
                "GitHub refused the stack merge. Check the stack's branch rules and merge \
                 requirements."
                    .into(),
            )),
            _ => Err(invalid()),
        }
    }

    /// What rebasing the stack topped by `pr` needs, once GitHub confirms
    /// the stack is as the viewer saw it and every open layer's branch
    /// may be pushed to.
    pub async fn stack_rebase(
        &self,
        pr: &PullRequestRef,
        stack_number: u64,
        expected: &[StackHead],
    ) -> Result<StackRebase> {
        let stack = self.fresh_stack(pr).await?;
        let (stack, open) = affected_layers(stack, pr.number, stack_number, expected, false)?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct HeadRepository {
            viewer_permission: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Access {
            head_repository: Option<HeadRepository>,
            #[serde(default)]
            maintainer_can_modify: bool,
        }
        let prs: Vec<PullRequestRef> = open
            .iter()
            .map(|layer| pr.repo.pull_request(layer.number))
            .collect();
        let refused = || {
            Error::Refused(
                "You cannot update every branch in this stack. Check write access and fork \
                 maintainer permissions before retrying."
                    .into(),
            )
        };
        for answer in self.batched(&prs, "PullRequestBranchAccess").await? {
            let access = answer?
                .pointer("/repository/pullRequest")
                .cloned()
                .and_then(|raw| serde_json::from_value::<Access>(raw).ok())
                .ok_or_else(refused)?;
            let writable = access.head_repository.is_some_and(|repository| {
                access.maintainer_can_modify || can_write(repository.viewer_permission.as_deref())
            });
            if !writable {
                return Err(refused());
            }
        }
        let token = self.inner.tokens.get(&pr.repo.host).await?;
        let credential =
            base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
        Ok(StackRebase {
            remote_url: format!(
                "https://{}/{}/{}.git",
                pr.repo.host.to_ascii_lowercase(),
                pr.repo.owner,
                pr.repo.name
            ),
            auth_header: format!("AUTHORIZATION: basic {credential}"),
            base: stack.base,
            layers: open
                .into_iter()
                .filter_map(|layer| {
                    Some(RebaseLayer {
                        number: layer.number,
                        head_sha: layer.head_sha?,
                        branch: layer.head_branch,
                    })
                })
                .collect(),
        })
    }

    /// The stack as GitHub has it now, without details (an action's check).
    async fn fresh_stack(&self, pr: &PullRequestRef) -> Result<Option<Stack>> {
        pr_variables(pr)?;
        let response = self
            .rest(RestCall::get(
                &pr.repo.host,
                "stack",
                format!(
                    "repos/{}/{}/stacks?pull_request={}",
                    pr.repo.owner, pr.repo.name, pr.number
                ),
            ))
            .await;
        match response {
            Ok(response) => decode_stacks("stack", &response.body),
            Err(Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTED: &str = r#"[{"id": 12, "number": 3, "url": "https://api.github.com/x",
        "html_url": "https://github.com/o/n/stacks/3", "base": {"ref": "main"},
        "pull_requests": [
            {"number": 1, "head": {"ref": "a"}, "state": "closed", "merged_at": "2026-10-01T00:00:00Z"},
            {"number": 2, "head": {"ref": "b", "sha": "bbb"}, "state": "open", "draft": true, "title": "B"},
            {"number": 4, "head": {"ref": "c", "sha": "ccc"}, "state": "closed"}
        ]}]"#;

    #[test]
    fn decodes_the_first_stack_with_its_layers_bottom_to_top() {
        let stack = decode_stacks("stack", LISTED.as_bytes()).unwrap().unwrap();
        assert_eq!(
            (stack.id.as_str(), stack.number, stack.base.as_str()),
            ("12", 3, "main")
        );
        assert_eq!(stack.url, "https://github.com/o/n/stacks/3");
        let states: Vec<_> = stack.layers.iter().map(|l| (l.number, l.state)).collect();
        assert_eq!(
            states,
            [
                (1, PullRequestState::Merged),
                (2, PullRequestState::Open),
                (4, PullRequestState::Closed)
            ]
        );
        assert_eq!(stack.layers[1].title.as_deref(), Some("B"));
        assert_eq!(stack.layers[1].is_draft, Some(true));
        assert!(decode_stacks("stack", b"[]").unwrap().is_none());
        let named =
            r#"[{"node_id": "S_1", "number": 5, "url": "u", "base": "dev", "pull_requests": []}]"#;
        let stack = decode_stacks("stack", named.as_bytes()).unwrap().unwrap();
        assert_eq!((stack.id.as_str(), stack.base.as_str()), ("S_1", "dev"));
    }

    fn layer(number: u64, sha: &str, state: PullRequestState) -> StackLayer {
        StackLayer {
            number,
            title: None,
            is_draft: None,
            head_sha: Some(sha.into()),
            head_branch: format!("b{number}"),
            state,
        }
    }

    fn stack(layers: Vec<StackLayer>) -> Option<Stack> {
        Some(Stack {
            id: "1".into(),
            number: 3,
            url: String::new(),
            base: "main".into(),
            layers,
        })
    }

    fn heads(heads: &[(u64, &str)]) -> Vec<StackHead> {
        heads
            .iter()
            .map(|&(number, sha)| StackHead {
                number,
                head_sha: sha.into(),
            })
            .collect()
    }

    #[test]
    fn an_action_runs_only_on_the_stack_the_viewer_saw() {
        use PullRequestState::*;
        let layers = || {
            vec![
                layer(1, "a", Merged),
                layer(2, "b", Open),
                layer(4, "c", Open),
            ]
        };
        // Merging through #2 affects the open layers up to it.
        let (_, open) = affected_layers(stack(layers()), 2, 3, &heads(&[(2, "b")]), true).unwrap();
        assert_eq!(open.iter().map(|l| l.number).collect::<Vec<_>>(), [2]);
        // A rebase is run from the top, over every open layer.
        let both = heads(&[(2, "b"), (4, "c")]);
        assert!(affected_layers(stack(layers()), 4, 3, &both, false).is_ok());
        let refused = |result: Result<(Stack, Vec<StackLayer>)>| result.unwrap_err().to_string();
        assert_eq!(
            refused(affected_layers(stack(layers()), 2, 3, &both, false)),
            "The stack changed. Refresh it before trying again."
        );
        // A head moved, another stack, a layer gone.
        assert!(
            affected_layers(stack(layers()), 4, 3, &heads(&[(2, "x"), (4, "c")]), false).is_err()
        );
        assert!(affected_layers(stack(layers()), 4, 9, &both, false).is_err());
        assert!(affected_layers(None, 4, 3, &both, false).is_err());
        // Something closed in between cannot be merged or rebased.
        let mut closed = layers();
        closed[1].state = Closed;
        assert_eq!(
            refused(affected_layers(
                stack(closed),
                4,
                3,
                &heads(&[(2, "b"), (4, "c")]),
                false
            )),
            "This operation is not supported for this stack."
        );
    }
}
