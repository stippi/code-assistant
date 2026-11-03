//! Changed files: the REST files API rebuilt as patch text, and which
//! files the viewer has marked viewed.

use super::{Connection, trimmed};
use crate::model::{FileViewed, FileViewedState, OmittedFileStat};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct RawFile {
    filename: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    previous_filename: Option<String>,
    #[serde(default)]
    patch: Option<String>,
    #[serde(default)]
    additions: Option<u64>,
    #[serde(default)]
    deletions: Option<u64>,
}

/// A page of files rebuilt as one patch.
#[derive(Debug, PartialEq)]
pub(crate) struct FilesPatch {
    pub patch: String,
    /// Some file's hunks were withheld (binary, too large).
    pub truncated: bool,
    /// Entries GitHub sent, decodable or not, so paging does not stop early.
    pub raw_count: usize,
    pub omitted_file_stats: Vec<OmittedFileStat>,
}

/// Quotes a path the way git does in patch headers, when it must.
pub(crate) fn quote_patch_path(path: &str) -> String {
    let mut body = String::with_capacity(path.len());
    let mut quoting = false;
    for c in path.chars() {
        let escape = match c {
            '"' => Some("\\\""),
            '\\' => Some("\\\\"),
            '\u{7}' => Some("\\a"),
            '\u{8}' => Some("\\b"),
            '\t' => Some("\\t"),
            '\n' => Some("\\n"),
            '\u{b}' => Some("\\v"),
            '\u{c}' => Some("\\f"),
            '\r' => Some("\\r"),
            _ => None,
        };
        if let Some(escape) = escape {
            body.push_str(escape);
            quoting = true;
        } else if (c as u32) < 0x20 || c as u32 == 0x7f {
            body.push_str(&format!("\\{:03o}", c as u32));
            quoting = true;
        } else {
            body.push(c);
        }
    }
    if quoting { format!("\"{body}\"") } else { body }
}

/// The files API omits `diff --git` headers; they are rebuilt so the
/// result reads like `git diff` output.
pub(crate) fn files_patch(entries: Vec<Value>) -> FilesPatch {
    let raw_count = entries.len();
    let mut patch = String::new();
    let mut truncated = false;
    let mut omitted_file_stats = Vec::new();
    for entry in entries {
        let Ok(file) = serde_json::from_value::<RawFile>(entry) else {
            continue;
        };
        let hunks = file.patch.unwrap_or_default();
        let status = file
            .status
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase());
        let status = status.as_deref();
        if hunks.is_empty() {
            // A rename without hunks is complete; changed lines without hunks are a gap.
            let additions = file.additions.unwrap_or(0);
            let deletions = file.deletions.unwrap_or(0);
            if additions + deletions > 0 {
                truncated = true;
                omitted_file_stats.push(OmittedFileStat {
                    path: file.filename.clone(),
                    additions,
                    deletions,
                });
            }
        }
        let old_path = match (status, file.previous_filename.as_deref()) {
            (Some("renamed"), Some(previous)) if !previous.is_empty() => previous.to_string(),
            _ => file.filename.clone(),
        };
        let new_path = &file.filename;
        patch.push_str(&format!(
            "diff --git {} {}\n",
            quote_patch_path(&format!("a/{old_path}")),
            quote_patch_path(&format!("b/{new_path}"))
        ));
        match status {
            Some("added") => patch.push_str("new file mode 100644\n"),
            Some("removed") => patch.push_str("deleted file mode 100644\n"),
            Some("renamed") => {
                patch.push_str(&format!("rename from {}\n", quote_patch_path(&old_path)));
                patch.push_str(&format!("rename to {}\n", quote_patch_path(new_path)));
            }
            _ => {}
        }
        let old_side = if status == Some("added") {
            "/dev/null".to_string()
        } else {
            quote_patch_path(&format!("a/{old_path}"))
        };
        let new_side = if status == Some("removed") {
            "/dev/null".to_string()
        } else {
            quote_patch_path(&format!("b/{new_path}"))
        };
        patch.push_str(&format!("--- {old_side}\n+++ {new_side}\n"));
        if !hunks.is_empty() {
            patch.push_str(&hunks);
            if !hunks.ends_with('\n') {
                patch.push('\n');
            }
        }
    }
    FilesPatch {
        patch,
        truncated,
        raw_count,
        omitted_file_stats,
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct FilesViewedData {
    repository: Option<RawFilesRepository>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawFilesRepository {
    pull_request: Option<RawFilesPullRequest>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawFilesPullRequest {
    files: Connection<RawFileViewed>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawFileViewed {
    path: String,
    viewer_viewed_state: Option<String>,
}

/// One page of viewed states; an unknown state reads as unviewed.
pub(crate) fn files_viewed_page(data: FilesViewedData) -> (Vec<FileViewed>, Option<String>) {
    let files = data
        .repository
        .and_then(|r| r.pull_request)
        .map(|pr| pr.files)
        .unwrap_or_default();
    let viewed = files
        .nodes()
        .filter(|node| !node.path.is_empty())
        .map(|node| FileViewed {
            path: node.path.clone(),
            state: match trimmed(node.viewer_viewed_state.as_deref())
                .map(|s| s.to_ascii_uppercase())
                .as_deref()
            {
                Some("VIEWED") => FileViewedState::Viewed,
                Some("DISMISSED") => FileViewedState::Dismissed,
                _ => FileViewedState::Unviewed,
            },
        })
        .collect();
    (viewed, files.next_cursor())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn patch_of(entries: Value) -> FilesPatch {
        files_patch(serde_json::from_value(entries).unwrap())
    }

    #[test]
    fn quotes_literal_backslashes_without_interpreting_them() {
        let result = patch_of(json!([
            {"filename": r"src\notes.ts", "status": "modified", "patch": "@@ -1 +1 @@\n-old\n+new"}
        ]));
        assert_eq!(
            result.patch,
            [
                r#"diff --git "a/src\\notes.ts" "b/src\\notes.ts""#,
                r#"--- "a/src\\notes.ts""#,
                r#"+++ "b/src\\notes.ts""#,
                "@@ -1 +1 @@",
                "-old",
                "+new",
                "",
            ]
            .join("\n")
        );
    }

    #[test]
    fn preserves_spaces_and_backslashes_in_both_rename_paths() {
        let result = patch_of(json!([
            {"previous_filename": r" old\name.ts ", "filename": r" new\name.ts ", "status": "renamed"}
        ]));
        assert_eq!(
            result.patch,
            [
                r#"diff --git "a/ old\\name.ts " "b/ new\\name.ts ""#,
                r#"rename from " old\\name.ts ""#,
                r#"rename to " new\\name.ts ""#,
                r#"--- "a/ old\\name.ts ""#,
                r#"+++ "b/ new\\name.ts ""#,
                "",
            ]
            .join("\n")
        );
        assert!(!result.truncated);
    }

    #[test]
    fn points_added_and_removed_files_at_dev_null() {
        let result = patch_of(json!([
            {"filename": "src/new.ts", "status": "added", "patch": "@@ -0,0 +1 @@\n+hello"},
            {"filename": "src/gone.ts", "status": "removed", "patch": "@@ -1 +0,0 @@\n-bye\n"}
        ]));
        assert_eq!(
            result.patch,
            [
                "diff --git a/src/new.ts b/src/new.ts",
                "new file mode 100644",
                "--- /dev/null",
                "+++ b/src/new.ts",
                "@@ -0,0 +1 @@",
                "+hello",
                "diff --git a/src/gone.ts b/src/gone.ts",
                "deleted file mode 100644",
                "--- a/src/gone.ts",
                "+++ /dev/null",
                "@@ -1 +0,0 @@",
                "-bye",
                "",
            ]
            .join("\n")
        );
    }

    #[test]
    fn names_both_paths_of_a_rename() {
        let result = patch_of(json!([
            {"filename": "src/new.ts", "status": "renamed", "previous_filename": "src/old.ts", "patch": "@@ -1 +1 @@\n-old\n+new"}
        ]));
        assert!(result.patch.starts_with(
            "diff --git a/src/old.ts b/src/new.ts\nrename from src/old.ts\nrename to src/new.ts\n--- a/src/old.ts\n+++ b/src/new.ts\n"
        ));
    }

    #[test]
    fn lists_a_file_without_hunks_and_says_what_was_withheld() {
        let result = patch_of(json!([
            {"filename": "logo.png", "status": "modified", "additions": 4, "deletions": 2},
            {"filename": "src/app.ts", "status": "modified", "additions": 1, "deletions": 1, "patch": "@@ -1 +1 @@\n-old\n+new"},
            {"no": "filename"}
        ]));
        assert!(
            result
                .patch
                .contains("diff --git a/logo.png b/logo.png\n--- a/logo.png\n+++ b/logo.png\n")
        );
        assert!(
            result
                .patch
                .contains("diff --git a/src/app.ts b/src/app.ts")
        );
        assert!(result.truncated);
        assert_eq!(result.raw_count, 3);
        assert_eq!(
            result.omitted_file_stats,
            [OmittedFileStat {
                path: "logo.png".into(),
                additions: 4,
                deletions: 2
            }]
        );
    }

    #[test]
    fn reads_an_unknown_viewed_state_as_unviewed() {
        let data = serde_json::from_value(json!({"repository": {"pullRequest": {"files": {
            "pageInfo": {"hasNextPage": true, "endCursor": "next"},
            "nodes": [
                {"path": "a.rs", "viewerViewedState": "VIEWED"},
                {"path": "b.rs", "viewerViewedState": "DISMISSED"},
                {"path": "c.rs", "viewerViewedState": "SOMETHING_NEW"},
                {"path": "", "viewerViewedState": "VIEWED"}
            ]
        }}}}))
        .unwrap();
        let (files, next) = files_viewed_page(data);
        let states: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.state)).collect();
        assert_eq!(
            states,
            [
                ("a.rs", FileViewedState::Viewed),
                ("b.rs", FileViewedState::Dismissed),
                ("c.rs", FileViewedState::Unviewed),
            ]
        );
        assert_eq!(next.as_deref(), Some("next"));
    }
}
