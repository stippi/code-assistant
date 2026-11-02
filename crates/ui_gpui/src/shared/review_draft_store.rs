//! Where the unsent texts of pull requests stay across restarts: one file
//! for the app, since a pull request belongs to no session.

use anyhow::Result;
use code_assistant_core::persistence::SessionLayout;
use code_assistant_core::utils::file_utils::atomic_write_json;
use github::PullRequestRef;
use github::model::{DiffSide, ReviewCommentDraft};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A line comment of the pending review, anchored like GitHub wants it:
/// `line` on `side` (LEFT the old file, RIGHT the new one), a range from
/// `start_line` on `start_side`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingComment {
    /// Unique in the app while it runs; given when added or loaded.
    #[serde(skip)]
    pub id: u64,
    pub path: String,
    pub side: DiffSide,
    pub line: u64,
    pub start_side: Option<DiffSide>,
    pub start_line: Option<u64>,
    pub body: String,
    /// The commented rows as shown, `+`/`-`/` ` prefixed.
    pub excerpt: String,
}

impl PendingComment {
    pub fn draft(&self) -> ReviewCommentDraft {
        ReviewCommentDraft {
            path: self.path.clone(),
            line: self.line,
            side: self.side,
            start_line: self.start_line,
            start_side: self.start_side,
            body: self.body.clone(),
        }
    }
}

/// What is written but not sent on one pull request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestDraft {
    #[serde(default)]
    pub comment: String,
    /// The text of the review.
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub pending: Vec<PendingComment>,
}

impl PullRequestDraft {
    pub fn is_empty(&self) -> bool {
        self.comment.is_empty() && self.summary.is_empty() && self.pending.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedReviewDraft {
    pub pr: PullRequestRef,
    #[serde(flatten)]
    pub draft: PullRequestDraft,
}

pub trait ReviewDraftStore: Send + Sync {
    /// All stored drafts; none before the first save.
    fn load(&self) -> Result<Vec<SavedReviewDraft>>;

    /// Store these drafts, replacing all others.
    fn save(&self, drafts: &[SavedReviewDraft]) -> Result<()>;
}

/// The drafts as `pull-request-drafts.json` next to the sessions.
pub struct FileReviewDraftStore {
    path: PathBuf,
}

impl FileReviewDraftStore {
    pub fn new(layout: &SessionLayout) -> Self {
        Self {
            path: layout.sessions_dir().join("pull-request-drafts.json"),
        }
    }
}

impl ReviewDraftStore for FileReviewDraftStore {
    fn load(&self) -> Result<Vec<SavedReviewDraft>> {
        let json = match std::fs::read_to_string(&self.path) {
            Ok(json) => json,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        Ok(serde_json::from_str(&json)?)
    }

    fn save(&self, drafts: &[SavedReviewDraft]) -> Result<()> {
        atomic_write_json(&self.path, drafts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use github::RepoRef;

    #[test]
    fn drafts_come_back_from_the_file_without_their_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileReviewDraftStore::new(&SessionLayout::new(dir.path().to_path_buf()));
        assert_eq!(store.load().unwrap(), []);

        let saved = SavedReviewDraft {
            pr: RepoRef::new("github.example.com", "o", "r").pull_request(7),
            draft: PullRequestDraft {
                comment: "Almost".into(),
                summary: String::new(),
                pending: vec![PendingComment {
                    id: 41,
                    path: "a.rs".into(),
                    side: DiffSide::Left,
                    line: 3,
                    start_side: None,
                    start_line: None,
                    body: "Why?".into(),
                    excerpt: "-x".into(),
                }],
            },
        };
        store.save(std::slice::from_ref(&saved)).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded[0].pr, saved.pr);
        assert_eq!(loaded[0].draft.comment, "Almost");
        assert_eq!(loaded[0].draft.pending[0].id, 0);
        assert_eq!(loaded[0].draft.pending[0].body, "Why?");
    }
}
