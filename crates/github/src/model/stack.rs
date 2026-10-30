use super::{MergeMethod, PullRequestState};
use serde::{Deserialize, Serialize};

/// A stack of pull requests: each layer's base is the head of the one
/// below, the bottom one's is `base`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stack {
    pub id: String,
    pub number: u64,
    pub url: String,
    pub base: String,
    /// Bottom to top.
    pub layers: Vec<StackLayer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackLayer {
    pub number: u64,
    /// The stack's listing leaves these out; its own read has them.
    pub title: Option<String>,
    pub is_draft: Option<bool>,
    pub head_sha: Option<String>,
    pub head_branch: String,
    pub state: PullRequestState,
}

/// Where a listed pull request sits in its stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackMembership {
    pub number: u64,
    /// 1 for the bottom layer.
    pub position: u64,
    pub size: u64,
    pub base: String,
}

/// A layer's head as the viewer saw it: a stack action refuses to run
/// when GitHub's differs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackHead {
    pub number: u64,
    pub head_sha: String,
}

/// What can be done to a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "action", content = "method")]
pub enum StackAction {
    /// Merge the stack through one layer: GitHub merges the layers below
    /// too and rebases the rest.
    Merge(MergeMethod),
    /// Rebase every open layer onto the one below, bottom to top.
    Rebase,
}

/// What a stack rebase needs: the repository to fetch from and push to,
/// how to sign in there, and the open layers bottom to top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackRebase {
    pub remote_url: String,
    /// An `http.extraheader` value carrying the credential.
    pub auth_header: String,
    pub base: String,
    pub layers: Vec<RebaseLayer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseLayer {
    pub number: u64,
    pub branch: String,
    pub head_sha: String,
}
