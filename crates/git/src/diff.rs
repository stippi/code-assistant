use crate::repository::GitRepository;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Upper bound on the size (in bytes) of a single side of a diff we will load
/// into memory and hand to the UI. Larger blobs are reported as `too_large`
/// and their text is omitted.
const MAX_DIFF_BYTES: usize = 1_500_000;

/// How a file changed relative to the comparison base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
}

/// A single changed file in a review listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    /// Repo-relative path of the file in its new/current location
    /// (forward-slash separated).
    pub path: String,
    /// For renames/copies, the original repo-relative path; otherwise `None`.
    pub orig_path: Option<String>,
    /// The kind of change.
    pub status: ChangeStatus,
}

/// Aggregate line-change counts for a review listing (à la `git diff --stat`).
///
/// Untracked files are not included — `git diff --numstat` only covers
/// tracked content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiffStats {
    pub additions: usize,
    pub deletions: usize,
}

/// The two whole-file sides of a diff, ready to be fed to the UI's unified
/// diff renderer. Either side may be `None` (pure add or delete). When
/// `is_binary` or `too_large` is set, the text sides are omitted and the UI
/// should show a placeholder instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiffContent {
    /// The old (base) content, or `None` if the file was added.
    pub old_text: Option<String>,
    /// The new (current) content, or `None` if the file was deleted.
    pub new_text: Option<String>,
    /// True if either side is not valid UTF-8 (i.e. a binary blob).
    pub is_binary: bool,
    /// True if either side exceeds [`MAX_DIFF_BYTES`].
    pub too_large: bool,
}

/// One side of a diff, before decoding.
enum Side {
    /// The side does not exist (pure add or delete).
    Absent,
    /// The side exists but is larger than [`MAX_DIFF_BYTES`].
    TooLarge,
    /// The raw bytes of the side.
    Bytes(Vec<u8>),
}

impl Side {
    fn from_bytes(bytes: Vec<u8>) -> Self {
        if bytes.len() > MAX_DIFF_BYTES {
            Side::TooLarge
        } else {
            Side::Bytes(bytes)
        }
    }
}

impl GitRepository {
    /// List files that differ between the working tree (including staged
    /// changes and untracked files) and `HEAD`.
    pub async fn changed_files_working_tree(&self) -> Result<Vec<ChangedFile>> {
        let out = self
            .git
            .run_bytes(
                self.workdir(),
                &[
                    "-c",
                    "core.quotepath=false",
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--untracked-files=all",
                ],
            )
            .await?;
        Ok(parse_status_z(&out))
    }

    /// List files that differ between `base` and `HEAD` using three-dot
    /// (merge-base) semantics, matching what a pull request would show.
    pub async fn changed_files_vs_base(&self, base: &str) -> Result<Vec<ChangedFile>> {
        let range = format!("{base}...HEAD");
        let out = self
            .git
            .run_bytes(
                self.workdir(),
                &[
                    "-c",
                    "core.quotepath=false",
                    "diff",
                    "--name-status",
                    "-M",
                    "-z",
                    &range,
                ],
            )
            .await?;
        Ok(parse_diff_name_status_z(&out))
    }

    /// Load both sides of the diff for `file` in working-tree mode:
    /// old = the `HEAD` version, new = the current working-tree file.
    pub async fn file_diff_working_tree(&self, file: &ChangedFile) -> Result<FileDiffContent> {
        let old_ref_path = file.orig_path.as_deref().unwrap_or(&file.path);
        let old = if matches!(file.status, ChangeStatus::Added | ChangeStatus::Untracked) {
            Side::Absent
        } else {
            let bytes = self
                .git
                .run_bytes(self.workdir(), &["show", &format!("HEAD:{old_ref_path}")])
                .await
                .with_context(|| format!("reading HEAD:{old_ref_path}"))?;
            Side::from_bytes(bytes)
        };

        let new = if matches!(file.status, ChangeStatus::Deleted) {
            Side::Absent
        } else {
            self.read_workdir_file(&file.path).await?
        };

        Ok(build_diff_content(old, new))
    }

    /// Load both sides of the diff for `file` in branch-vs-base mode:
    /// old = the version at `merge-base(base, HEAD)`, new = the `HEAD` version.
    pub async fn file_diff_vs_base(
        &self,
        base: &str,
        file: &ChangedFile,
    ) -> Result<FileDiffContent> {
        let merge_base = self
            .git
            .run(self.workdir(), &["merge-base", base, "HEAD"])
            .await
            .with_context(|| format!("merge-base {base} HEAD"))?;
        let merge_base = merge_base.trim();

        let old_ref_path = file.orig_path.as_deref().unwrap_or(&file.path);
        let old = if matches!(file.status, ChangeStatus::Added) {
            Side::Absent
        } else {
            let bytes = self
                .git
                .run_bytes(
                    self.workdir(),
                    &["show", &format!("{merge_base}:{old_ref_path}")],
                )
                .await
                .with_context(|| format!("reading {merge_base}:{old_ref_path}"))?;
            Side::from_bytes(bytes)
        };

        let new = if matches!(file.status, ChangeStatus::Deleted) {
            Side::Absent
        } else {
            let bytes = self
                .git
                .run_bytes(self.workdir(), &["show", &format!("HEAD:{}", file.path)])
                .await
                .with_context(|| format!("reading HEAD:{}", file.path))?;
            Side::from_bytes(bytes)
        };

        Ok(build_diff_content(old, new))
    }

    /// Aggregate added/deleted line counts of the working tree (staged +
    /// unstaged) vs `HEAD`. Untracked files are not counted.
    pub async fn diff_stats_working_tree(&self) -> Result<DiffStats> {
        let out = self
            .git
            .run_bytes(self.workdir(), &["diff", "--numstat", "-M", "HEAD"])
            .await?;
        Ok(parse_numstat(&out))
    }

    /// Aggregate added/deleted line counts of `base...HEAD` (merge-base
    /// semantics, matching [`Self::changed_files_vs_base`]).
    pub async fn diff_stats_vs_base(&self, base: &str) -> Result<DiffStats> {
        let range = format!("{base}...HEAD");
        let out = self
            .git
            .run_bytes(self.workdir(), &["diff", "--numstat", "-M", &range])
            .await?;
        Ok(parse_numstat(&out))
    }

    /// Candidate base refs for branch-vs-base comparison: local branches plus
    /// remote-tracking branches (excluding `*/HEAD`), sorted and de-duplicated.
    pub fn list_base_candidates(&self) -> Result<Vec<String>> {
        let repo = self.repo.to_thread_local();
        let mut out = Vec::new();

        for reference in repo.references()?.local_branches()? {
            let reference = reference.map_err(|e| anyhow::anyhow!("{e}"))?;
            let name = reference.name().shorten().to_string();
            if !name.is_empty() {
                out.push(name);
            }
        }

        for reference in repo.references()?.remote_branches()? {
            let reference = reference.map_err(|e| anyhow::anyhow!("{e}"))?;
            let name = reference.name().shorten().to_string();
            if name.is_empty() || name.ends_with("/HEAD") {
                continue;
            }
            out.push(name);
        }

        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Read a working-tree file as a diff `Side`, mapping a missing file to
    /// `Absent` and an oversized file to `TooLarge` (without reading it).
    async fn read_workdir_file(&self, rel_path: &str) -> Result<Side> {
        let full = self.workdir().join(rel_path);
        match tokio::fs::metadata(&full).await {
            Ok(meta) if meta.len() as usize > MAX_DIFF_BYTES => Ok(Side::TooLarge),
            Ok(_) => match tokio::fs::read(&full).await {
                Ok(bytes) => Ok(Side::from_bytes(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Side::Absent),
                Err(e) => Err(e).with_context(|| format!("reading {}", full.display())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Side::Absent),
            Err(e) => Err(e).with_context(|| format!("stat {}", full.display())),
        }
    }
}

/// Classify a porcelain `XY` status pair into a single net `ChangeStatus`.
fn classify_status(x: char, y: char) -> ChangeStatus {
    if x == '?' || y == '?' {
        return ChangeStatus::Untracked;
    }
    let has = |c: char| x == c || y == c;
    if has('R') {
        ChangeStatus::Renamed
    } else if has('C') {
        ChangeStatus::Copied
    } else if has('A') {
        ChangeStatus::Added
    } else if has('D') {
        ChangeStatus::Deleted
    } else if has('T') {
        ChangeStatus::TypeChanged
    } else {
        ChangeStatus::Modified
    }
}

/// Parse the output of `git status --porcelain=v1 -z`.
///
/// Records are NUL-terminated. Each record is `XY<space>PATH`. For renames and
/// copies the `-z` format omits the ` -> ` and reverses the field order, so the
/// new path is in the record itself and the original path is the *next*
/// NUL-terminated field.
fn parse_status_z(bytes: &[u8]) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    let mut chunks = bytes.split(|&b| b == 0);
    while let Some(chunk) = chunks.next() {
        if chunk.len() < 4 {
            // Empty trailing chunk or malformed record; skip.
            continue;
        }
        let x = chunk[0] as char;
        let y = chunk[1] as char;
        // chunk[2] is the separating space; path starts at index 3.
        let path = String::from_utf8_lossy(&chunk[3..]).into_owned();
        let status = classify_status(x, y);
        let orig_path = if matches!(status, ChangeStatus::Renamed | ChangeStatus::Copied) {
            chunks
                .next()
                .map(|c| String::from_utf8_lossy(c).into_owned())
        } else {
            None
        };
        files.push(ChangedFile {
            path,
            orig_path,
            status,
        });
    }
    files
}

/// Parse the output of `git diff --name-status -M -z`.
///
/// Fields are NUL-terminated. A regular record is `STATUS`, then `PATH`. A
/// rename/copy record is `STATUS`, then the source path, then the destination
/// path.
fn parse_diff_name_status_z(bytes: &[u8]) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    let mut chunks = bytes.split(|&b| b == 0).filter(|c| !c.is_empty());
    while let Some(status_chunk) = chunks.next() {
        let code = status_chunk[0] as char;
        let status = match code {
            'A' => ChangeStatus::Added,
            'D' => ChangeStatus::Deleted,
            'R' => ChangeStatus::Renamed,
            'C' => ChangeStatus::Copied,
            'T' => ChangeStatus::TypeChanged,
            _ => ChangeStatus::Modified,
        };
        if matches!(status, ChangeStatus::Renamed | ChangeStatus::Copied) {
            let Some(old) = chunks.next() else { break };
            let Some(new) = chunks.next() else { break };
            files.push(ChangedFile {
                path: String::from_utf8_lossy(new).into_owned(),
                orig_path: Some(String::from_utf8_lossy(old).into_owned()),
                status,
            });
        } else {
            let Some(path) = chunks.next() else { break };
            files.push(ChangedFile {
                path: String::from_utf8_lossy(path).into_owned(),
                orig_path: None,
                status,
            });
        }
    }
    files
}

/// Parse `git diff --numstat` output and sum the per-file counts.
///
/// Each line is `ADDED<TAB>DELETED<TAB>PATH`; binary files report `-` in the
/// numeric columns and are skipped.
fn parse_numstat(bytes: &[u8]) -> DiffStats {
    let mut stats = DiffStats::default();
    for line in bytes.split(|&b| b == b'\n') {
        let mut fields = line.split(|&b| b == b'\t');
        let (Some(add), Some(del)) = (fields.next(), fields.next()) else {
            continue;
        };
        let parse = |f: &[u8]| std::str::from_utf8(f).ok()?.parse::<usize>().ok();
        if let (Some(add), Some(del)) = (parse(add), parse(del)) {
            stats.additions += add;
            stats.deletions += del;
        }
    }
    stats
}

/// Decode both raw sides into a [`FileDiffContent`], flagging binary and
/// oversized content.
fn build_diff_content(old: Side, new: Side) -> FileDiffContent {
    let mut is_binary = false;
    let mut too_large = false;

    let mut decode = |side: Side| -> Option<String> {
        match side {
            Side::Absent => None,
            Side::TooLarge => {
                too_large = true;
                None
            }
            Side::Bytes(bytes) => match String::from_utf8(bytes) {
                Ok(text) => Some(text),
                Err(_) => {
                    is_binary = true;
                    None
                }
            },
        }
    };

    let old_text = decode(old);
    let new_text = decode(new);

    FileDiffContent {
        old_text,
        new_text,
        is_binary,
        too_large,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::init_repo_with_commit;
    use std::path::Path;
    use tempfile::TempDir;

    /// Run a git command synchronously for test setup (staging, committing).
    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(status.success(), "git {args:?} failed");
    }

    fn write(dir: &Path, rel: &str, content: &[u8]) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).unwrap();
    }

    fn find<'a>(files: &'a [ChangedFile], path: &str) -> &'a ChangedFile {
        files
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("no changed file with path {path} in {files:?}"))
    }

    #[test]
    fn parse_status_handles_rename_pair() {
        // Rename record: new path in-record, old path in following field.
        let raw = b"R  new_name.txt\0old_name.txt\0M  other.txt\0";
        let files = parse_status_z(raw);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].status, ChangeStatus::Renamed);
        assert_eq!(files[0].path, "new_name.txt");
        assert_eq!(files[0].orig_path.as_deref(), Some("old_name.txt"));
        assert_eq!(files[1].status, ChangeStatus::Modified);
        assert_eq!(files[1].path, "other.txt");
        assert_eq!(files[1].orig_path, None);
    }

    #[test]
    fn parse_status_handles_untracked_and_paths_with_spaces() {
        let raw = b"?? a file.txt\0 M tracked.rs\0";
        let files = parse_status_z(raw);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].status, ChangeStatus::Untracked);
        assert_eq!(files[0].path, "a file.txt");
        assert_eq!(files[1].status, ChangeStatus::Modified);
        assert_eq!(files[1].path, "tracked.rs");
    }

    #[test]
    fn parse_diff_name_status_handles_rename() {
        let raw = b"M\0a.txt\0R100\0old.txt\0new.txt\0A\0added.txt\0";
        let files = parse_diff_name_status_z(raw);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].status, ChangeStatus::Modified);
        assert_eq!(files[0].path, "a.txt");
        assert_eq!(files[1].status, ChangeStatus::Renamed);
        assert_eq!(files[1].orig_path.as_deref(), Some("old.txt"));
        assert_eq!(files[1].path, "new.txt");
        assert_eq!(files[2].status, ChangeStatus::Added);
        assert_eq!(files[2].path, "added.txt");
    }

    #[test]
    fn parse_numstat_sums_and_skips_binary() {
        let raw = b"3\t1\tsrc/a.rs\n-\t-\tblob.bin\n10\t0\tnew.txt\n";
        let stats = parse_numstat(raw);
        assert_eq!(
            stats,
            DiffStats {
                additions: 13,
                deletions: 1
            }
        );
        assert_eq!(parse_numstat(b""), DiffStats::default());
    }

    #[tokio::test]
    async fn diff_stats_working_tree_counts_lines() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());

        write(dir.path(), "a.txt", b"one\ntwo\nthree\n");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-m", "seed"]);

        // Replace one line and add one (net: +2 -1).
        write(dir.path(), "a.txt", b"one\nTWO\nthree\nfour\n");

        let repo = GitRepository::open(dir.path()).unwrap();
        let stats = repo.diff_stats_working_tree().await.unwrap();
        assert_eq!(
            stats,
            DiffStats {
                additions: 2,
                deletions: 1
            }
        );
    }

    #[tokio::test]
    async fn working_tree_add_modify_delete_untracked() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());

        // Seed two tracked files and commit them.
        write(dir.path(), "keep.txt", b"one\ntwo\n");
        write(dir.path(), "gone.txt", b"delete me\n");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-m", "seed"]);

        // Modify keep.txt, delete gone.txt, add an untracked new.txt.
        write(dir.path(), "keep.txt", b"one\nchanged\n");
        std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
        write(dir.path(), "new.txt", b"brand new\n");

        let repo = GitRepository::open(dir.path()).unwrap();
        let files = repo.changed_files_working_tree().await.unwrap();

        assert_eq!(find(&files, "keep.txt").status, ChangeStatus::Modified);
        assert_eq!(find(&files, "gone.txt").status, ChangeStatus::Deleted);
        assert_eq!(find(&files, "new.txt").status, ChangeStatus::Untracked);

        // Modified: both sides present.
        let d = repo
            .file_diff_working_tree(find(&files, "keep.txt"))
            .await
            .unwrap();
        assert_eq!(d.old_text.as_deref(), Some("one\ntwo\n"));
        assert_eq!(d.new_text.as_deref(), Some("one\nchanged\n"));
        assert!(!d.is_binary && !d.too_large);

        // Deleted: no new side.
        let d = repo
            .file_diff_working_tree(find(&files, "gone.txt"))
            .await
            .unwrap();
        assert_eq!(d.old_text.as_deref(), Some("delete me\n"));
        assert_eq!(d.new_text, None);

        // Untracked: no old side.
        let d = repo
            .file_diff_working_tree(find(&files, "new.txt"))
            .await
            .unwrap();
        assert_eq!(d.old_text, None);
        assert_eq!(d.new_text.as_deref(), Some("brand new\n"));
    }

    #[tokio::test]
    async fn working_tree_rename() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());

        write(dir.path(), "original.txt", b"stable content\n");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-m", "seed"]);

        // Stage a rename so git detects it as R.
        git(dir.path(), &["mv", "original.txt", "renamed.txt"]);

        let repo = GitRepository::open(dir.path()).unwrap();
        let files = repo.changed_files_working_tree().await.unwrap();

        let renamed = find(&files, "renamed.txt");
        assert_eq!(renamed.status, ChangeStatus::Renamed);
        assert_eq!(renamed.orig_path.as_deref(), Some("original.txt"));

        let d = repo.file_diff_working_tree(renamed).await.unwrap();
        assert_eq!(d.old_text.as_deref(), Some("stable content\n"));
        assert_eq!(d.new_text.as_deref(), Some("stable content\n"));
    }

    #[tokio::test]
    async fn working_tree_binary_and_too_large() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());

        // Binary file (invalid UTF-8 bytes).
        write(dir.path(), "blob.bin", &[0u8, 159, 146, 150, 255]);
        // Oversized text file.
        let big = vec![b'a'; MAX_DIFF_BYTES + 10];
        write(dir.path(), "big.txt", &big);

        let repo = GitRepository::open(dir.path()).unwrap();
        let files = repo.changed_files_working_tree().await.unwrap();

        let d = repo
            .file_diff_working_tree(find(&files, "blob.bin"))
            .await
            .unwrap();
        assert!(d.is_binary);
        assert_eq!(d.new_text, None);

        let d = repo
            .file_diff_working_tree(find(&files, "big.txt"))
            .await
            .unwrap();
        assert!(d.too_large);
        assert_eq!(d.new_text, None);
    }

    #[tokio::test]
    async fn branch_vs_base_three_dot() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());

        // Base commit on the default branch.
        write(dir.path(), "shared.txt", b"base line\n");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-m", "base"]);
        let base_branch = {
            let repo = GitRepository::open(dir.path()).unwrap();
            repo.current_branch().unwrap()
        };

        // Diverge onto a feature branch: modify shared, add a file.
        git(dir.path(), &["checkout", "-b", "feature"]);
        write(dir.path(), "shared.txt", b"feature line\n");
        write(dir.path(), "feature_only.txt", b"new on feature\n");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-m", "feature work"]);

        let repo = GitRepository::open(dir.path()).unwrap();
        let files = repo.changed_files_vs_base(&base_branch).await.unwrap();

        assert_eq!(find(&files, "shared.txt").status, ChangeStatus::Modified);
        assert_eq!(find(&files, "feature_only.txt").status, ChangeStatus::Added);

        let d = repo
            .file_diff_vs_base(&base_branch, find(&files, "shared.txt"))
            .await
            .unwrap();
        assert_eq!(d.old_text.as_deref(), Some("base line\n"));
        assert_eq!(d.new_text.as_deref(), Some("feature line\n"));

        let d = repo
            .file_diff_vs_base(&base_branch, find(&files, "feature_only.txt"))
            .await
            .unwrap();
        assert_eq!(d.old_text, None);
        assert_eq!(d.new_text.as_deref(), Some("new on feature\n"));
    }

    #[tokio::test]
    async fn base_candidates_include_branches() {
        let dir = TempDir::new().unwrap();
        init_repo_with_commit(dir.path());
        git(dir.path(), &["checkout", "-b", "extra-branch"]);

        let repo = GitRepository::open(dir.path()).unwrap();
        let candidates = repo.list_base_candidates().unwrap();
        assert!(candidates.iter().any(|c| c == "extra-branch"));
    }
}
