//! The GraphQL documents (ported from T3 Code) and what is sent of them.
//!
//! Each request carries one operation plus the fragments it spreads, with
//! comments stripped, rather than the whole file it is written in.

pub(crate) mod alias;
mod lexer;

use crate::error::GraphQlError;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

const DOCUMENTS: &[&str] = &[
    include_str!("reads.graphql"),
    include_str!("reviews.graphql"),
    include_str!("listings.graphql"),
    include_str!("batches.graphql"),
    include_str!("mutations.graphql"),
    include_str!("heads.graphql"),
];

/// Every operation by name, ready to send.
static OPERATIONS: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    let mut operations = HashMap::new();
    for document in DOCUMENTS {
        let definitions = lexer::definitions(document);
        let fragments: HashMap<&str, &str> = definitions
            .iter()
            .filter(|d| d.keyword == "fragment")
            .map(|d| (d.name.as_str(), d.text.as_str()))
            .collect();
        for definition in definitions.iter().filter(|d| d.keyword != "fragment") {
            let mut text = definition.text.clone();
            let mut included: Vec<String> = Vec::new();
            let mut pending = lexer::fragment_spreads(&definition.text);
            while let Some(name) = pending.pop() {
                if included.contains(&name) {
                    continue;
                }
                let fragment = fragments
                    .get(name.as_str())
                    .unwrap_or_else(|| panic!("fragment {name} is not defined"));
                pending.extend(lexer::fragment_spreads(fragment));
                text.push('\n');
                text.push_str(fragment);
                included.push(name);
            }
            operations.insert(definition.name.clone(), text);
        }
    }
    operations
});

/// The text sent for `name`: the operation and the fragments it uses.
pub(crate) fn operation(name: &str) -> &'static str {
    OPERATIONS
        .get(name)
        .unwrap_or_else(|| panic!("GraphQL operation {name} is not defined"))
}

/// A GraphQL answer: data, errors, or both.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Envelope {
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(default)]
    pub errors: Vec<GraphQlError>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_operation_is_complete() {
        for (name, text) in OPERATIONS.iter() {
            assert!(
                text.starts_with("query ") || text.starts_with("mutation "),
                "{name}"
            );
            assert!(!text.contains('#'), "{name} still has comments");
        }
    }

    #[test]
    fn an_operation_carries_the_fragments_it_spreads_transitively() {
        let core = operation("PullRequestCore");
        assert!(core.contains("fragment CheckContext on StatusCheckRollupContext"));
        let threads = operation("ReviewThreads");
        assert!(threads.contains("fragment ThreadComment on PullRequestReviewComment"));
        // ThreadComment spreads Reactions; ReviewThreads also spreads it itself.
        assert_eq!(
            threads.matches("fragment Reactions on Reactable").count(),
            1
        );
        assert!(!operation("PullRequestNodeId").contains("fragment"));
    }

    #[test]
    fn defines_every_operation_the_client_sends() {
        let sent = [
            "AddComment",
            "AddReaction",
            "AddReviewThreadReply",
            "ClosePullRequest",
            "ConvertPullRequestToDraft",
            "DeletePullRequestBranch",
            "DisablePullRequestAutoMerge",
            "EnablePullRequestAutoMerge",
            "LabelCandidates",
            "MarkPullRequestReadyForReview",
            "MergePullRequest",
            "PullRequestActionState",
            "PullRequestActivity",
            "PullRequestBranchToDelete",
            "PullRequestCheckContexts",
            "PullRequestCommitStats",
            "PullRequestCore",
            "PullRequestFilesViewed",
            "PullRequestList",
            "PullRequestNodeId",
            "PullRequestSearch",
            "PullRequestSourceBranch",
            "PullRequestStats",
            "PullRequestSubject",
            "PullRequestSummary",
            "PullRequestsByHead",
            "RemoveReaction",
            "ReopenPullRequest",
            "ResolveReviewThread",
            "ReviewDismissals",
            "ReviewThreadComments",
            "ReviewThreads",
            "ReviewerCandidates",
            "RoutingIdentityViewer",
            "SetFileViewed",
            "UnresolveReviewThread",
            "UpdateIssueComment",
            "UpdatePullRequest",
            "UpdatePullRequestBranch",
            "UpdateReviewComment",
        ];
        for name in sent {
            assert!(OPERATIONS.contains_key(name), "{name} is not defined");
        }
        assert_eq!(OPERATIONS.len(), sent.len(), "an operation is never sent");
        for batched in ["PullRequestSummary", "PullRequestStats", "SetFileViewed"] {
            alias::aliased_operation(operation(batched), 2);
        }
    }
}
