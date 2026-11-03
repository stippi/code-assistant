use serde::{Deserialize, Serialize};

/// A slice of a pull request's or commit's patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffPage {
    /// Unified diff text with `diff --git` headers.
    pub patch: String,
    /// Hunks were withheld within this slice (see `omitted_file_stats`).
    pub truncated: bool,
    /// Set while more slices follow.
    pub next_cursor: Option<String>,
    /// Files GitHub sent no hunks for although lines changed (binary, too large).
    pub omitted_file_stats: Vec<OmittedFileStat>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmittedFileStat {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileViewedState {
    Unviewed,
    Viewed,
    /// Viewed, but the file changed since.
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileViewed {
    pub path: String,
    pub state: FileViewedState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesViewed {
    /// Files GitHub reported a state for; a missing file is unviewed.
    pub files: Vec<FileViewed>,
    pub truncated: bool,
}

/// How a file changed, which says which sides have contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileChange {
    Modified,
    Renamed,
    Added,
    Deleted,
}

/// Both sides of one changed file, for expanding context around hunks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFileContents {
    pub old: String,
    pub new: String,
}
