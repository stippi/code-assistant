use serde::{Deserialize, Serialize};

/// A person, app or team as GitHub names them. Teams use `org/slug` as login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub is_bot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub name: String,
    /// Hex colour without `#`, as GitHub stores it.
    pub color: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PullRequestState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mergeability {
    Mergeable,
    Conflicting,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

/// How "Update branch" brings the base in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateMethod {
    Merge,
    Rebase,
}

/// The side of a diff a line belongs to: `Left` the old file, `Right` the new.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffSide {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReactionContent {
    ThumbsUp,
    ThumbsDown,
    Laugh,
    Hooray,
    Confused,
    Heart,
    Rocket,
    Eyes,
}

impl ReactionContent {
    pub const ALL: [ReactionContent; 8] = [
        ReactionContent::ThumbsUp,
        ReactionContent::ThumbsDown,
        ReactionContent::Laugh,
        ReactionContent::Hooray,
        ReactionContent::Confused,
        ReactionContent::Heart,
        ReactionContent::Rocket,
        ReactionContent::Eyes,
    ];

    /// GitHub's `ReactionContent` enum value.
    pub(crate) fn graphql_name(self) -> &'static str {
        match self {
            ReactionContent::ThumbsUp => "THUMBS_UP",
            ReactionContent::ThumbsDown => "THUMBS_DOWN",
            ReactionContent::Laugh => "LAUGH",
            ReactionContent::Hooray => "HOORAY",
            ReactionContent::Confused => "CONFUSED",
            ReactionContent::Heart => "HEART",
            ReactionContent::Rocket => "ROCKET",
            ReactionContent::Eyes => "EYES",
        }
    }

    pub(crate) fn from_graphql(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_uppercase();
        Self::ALL.into_iter().find(|c| c.graphql_name() == name)
    }
}

/// One reaction emoji on a subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    pub content: ReactionContent,
    /// Everyone who reacted, the viewer included.
    pub count: u32,
    /// Logins of up to ten reactors, without the viewer's own login.
    pub actors: Vec<String>,
    pub viewer_has_reacted: bool,
}
