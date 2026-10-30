//! Unified diff text, as `git diff` and GitHub print it, parsed into files
//! and hunks. Input from the network is not trusted to be well formed:
//! anything that does not parse is skipped, never a panic.

use crate::ChangeStatus;

/// What a hunk line does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchLineKind {
    Context,
    Added,
    Removed,
}

/// One line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchLine {
    pub kind: PatchLineKind,
    /// Without the `+`/`-`/` ` prefix and the line ending.
    pub text: String,
    /// A `\ No newline at end of file` followed: the line is the last of its
    /// side and has no line ending.
    pub missing_newline: bool,
}

/// One `@@ -old_start,old_lines +new_start,new_lines @@ section` hunk.
/// Start lines are 1-based; a side without lines starts at 0, as in the
/// header (`-0,0` for an added file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchHunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    /// The text after the closing `@@`, usually the enclosing function;
    /// empty when there is none.
    pub section: String,
    pub lines: Vec<PatchLine>,
}

/// One file of a multi-file patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    /// The file's path after the change; for a deletion, the deleted path.
    pub path: String,
    /// The previous path of a rename or copy.
    pub orig_path: Option<String>,
    pub status: ChangeStatus,
    /// Similarity of a rename or copy, in percent.
    pub similarity: Option<u8>,
    /// File modes (`100644`, …) the patch names: both for a mode change, the
    /// new one for an added file, the old one for a deleted file.
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub is_binary: bool,
    pub hunks: Vec<PatchHunk>,
}

/// Parse a multi-file patch (`diff --git` sections; plain `---`/`+++`
/// unified diffs work too).
pub fn parse_patch(text: &str) -> Vec<FilePatch> {
    let mut files = Vec::new();
    let mut file: Option<FileBuilder> = None;
    for line in text.lines() {
        // Inside a hunk its counts decide: "--- x" there is a removed line.
        if let Some(current) = &mut file
            && current.hunks.is_open()
            && current.hunks.take(line)
        {
            continue;
        }
        if let Some(paths) = line.strip_prefix("diff --git ") {
            files.extend(file.take().map(FileBuilder::finish));
            file = Some(FileBuilder::from_git_header(paths));
            continue;
        }
        // A plain unified diff starts each file with its `---` line.
        if line.starts_with("--- ") && file.as_ref().is_none_or(|f| !f.hunks.hunks.is_empty()) {
            files.extend(file.take().map(FileBuilder::finish));
            file = Some(FileBuilder::default());
        }
        if let Some(current) = &mut file {
            current.header_line(line);
        }
    }
    files.extend(file.map(FileBuilder::finish));
    files
}

/// Parse the hunks of a single file, e.g. a GitHub `files[].patch` field.
/// Anything before the first `@@` header is ignored.
pub fn parse_hunks(text: &str) -> Vec<PatchHunk> {
    let mut hunks = HunkReader::default();
    for line in text.lines() {
        hunks.take(line);
    }
    hunks.hunks
}

/// Collects a file's hunks line by line.
#[derive(Default)]
struct HunkReader {
    hunks: Vec<PatchHunk>,
    /// Lines the open hunk still expects on each side.
    old_left: usize,
    new_left: usize,
}

impl HunkReader {
    fn is_open(&self) -> bool {
        self.old_left > 0 || self.new_left > 0
    }

    /// Take `line` if it is a hunk header, a line of the open hunk or the
    /// no-newline marker after one; `false` leaves it to the caller.
    fn take(&mut self, line: &str) -> bool {
        if line.starts_with('\\') {
            // "\ No newline at end of file", after the line it is about.
            if let Some(last) = self.hunks.last_mut().and_then(|h| h.lines.last_mut()) {
                last.missing_newline = true;
            }
            return !self.hunks.is_empty();
        }
        if line.starts_with("@@") {
            match parse_hunk_header(line) {
                Some(hunk) => {
                    (self.old_left, self.new_left) = (hunk.old_lines, hunk.new_lines);
                    self.hunks.push(hunk);
                }
                // A broken header: its lines are skipped as stray text.
                None => (self.old_left, self.new_left) = (0, 0),
            }
            return true;
        }
        if !self.is_open() {
            return false;
        }
        let (kind, text) = match line.as_bytes().first() {
            // An empty context line whose single space was stripped.
            None => (PatchLineKind::Context, ""),
            Some(b' ') => (PatchLineKind::Context, &line[1..]),
            Some(b'+') => (PatchLineKind::Added, &line[1..]),
            Some(b'-') => (PatchLineKind::Removed, &line[1..]),
            Some(_) => {
                (self.old_left, self.new_left) = (0, 0);
                return false;
            }
        };
        let (old, new) = match kind {
            PatchLineKind::Context => (1, 1),
            PatchLineKind::Added => (0, 1),
            PatchLineKind::Removed => (1, 0),
        };
        // More lines than the header announced: the hunk is over.
        if self.old_left < old || self.new_left < new {
            (self.old_left, self.new_left) = (0, 0);
            return false;
        }
        self.old_left -= old;
        self.new_left -= new;
        if let Some(hunk) = self.hunks.last_mut() {
            hunk.lines.push(PatchLine {
                kind,
                text: text.to_string(),
                missing_newline: false,
            });
        }
        true
    }
}

/// `@@ -a[,b] +c[,d] @@[ section]`, without lines yet.
fn parse_hunk_header(line: &str) -> Option<PatchHunk> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, section) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |range: &str| -> Option<(usize, usize)> {
        match range.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((range.parse().ok()?, 1)),
        }
    };
    let (old_start, old_lines) = range(old)?;
    let (new_start, new_lines) = range(new)?;
    Some(PatchHunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        section: section.trim().to_string(),
        lines: Vec::new(),
    })
}

/// A file of [`parse_patch`] while its header and hunks are read.
#[derive(Default)]
struct FileBuilder {
    /// `None` once a header names `/dev/null`.
    old_path: Option<String>,
    new_path: Option<String>,
    added: bool,
    deleted: bool,
    renamed: bool,
    copied: bool,
    similarity: Option<u8>,
    old_mode: Option<String>,
    new_mode: Option<String>,
    is_binary: bool,
    /// Inside `GIT binary patch` data, which runs to the next file.
    in_binary_data: bool,
    hunks: HunkReader,
}

impl FileBuilder {
    fn from_git_header(paths: &str) -> Self {
        let (old_path, new_path) = split_git_paths(paths).unwrap_or_default();
        Self {
            old_path: Some(strip_prefix(old_path, "a/")),
            new_path: Some(strip_prefix(new_path, "b/")),
            ..Self::default()
        }
    }

    /// A line outside the hunks: an extended header, a file header, a hunk
    /// header or binary data.
    fn header_line(&mut self, line: &str) {
        if self.in_binary_data || self.hunks.take(line) {
            return;
        }
        let path = |rest: &str| unquote(rest).unwrap_or_else(|| rest.to_string());
        if let Some(rest) = line.strip_prefix("--- ") {
            self.old_path = file_header_path(rest, "a/");
            self.added |= self.old_path.is_none();
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            self.new_path = file_header_path(rest, "b/");
            self.deleted |= self.new_path.is_none();
        } else if let Some(mode) = line.strip_prefix("new file mode ") {
            self.added = true;
            self.new_mode = Some(mode.to_string());
        } else if let Some(mode) = line.strip_prefix("deleted file mode ") {
            self.deleted = true;
            self.old_mode = Some(mode.to_string());
        } else if let Some(mode) = line.strip_prefix("old mode ") {
            self.old_mode = Some(mode.to_string());
        } else if let Some(mode) = line.strip_prefix("new mode ") {
            self.new_mode = Some(mode.to_string());
        } else if let Some(rest) = line.strip_prefix("rename from ") {
            self.renamed = true;
            self.old_path = Some(path(rest));
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            self.renamed = true;
            self.new_path = Some(path(rest));
        } else if let Some(rest) = line.strip_prefix("copy from ") {
            self.copied = true;
            self.old_path = Some(path(rest));
        } else if let Some(rest) = line.strip_prefix("copy to ") {
            self.copied = true;
            self.new_path = Some(path(rest));
        } else if let Some(rest) = line.strip_prefix("similarity index ") {
            self.similarity = rest
                .strip_suffix('%')
                .and_then(|n| n.parse().ok())
                .filter(|n| *n <= 100);
        } else if let Some(rest) = line.strip_prefix("Binary files ") {
            self.is_binary = true;
            self.added |= rest.starts_with("/dev/null and ");
            self.deleted |= rest.ends_with(" and /dev/null differ");
        } else if line == "GIT binary patch" {
            self.is_binary = true;
            self.in_binary_data = true;
        }
    }

    fn finish(self) -> FilePatch {
        let status = if self.added {
            ChangeStatus::Added
        } else if self.deleted {
            ChangeStatus::Deleted
        } else if self.renamed {
            ChangeStatus::Renamed
        } else if self.copied {
            ChangeStatus::Copied
        } else {
            ChangeStatus::Modified
        };
        let (path, orig_path) = match status {
            ChangeStatus::Deleted => (self.old_path.or(self.new_path), None),
            ChangeStatus::Renamed | ChangeStatus::Copied => (self.new_path, self.old_path),
            _ => (self.new_path.or(self.old_path), None),
        };
        FilePatch {
            path: path.unwrap_or_default(),
            orig_path,
            status,
            similarity: self.similarity,
            old_mode: self.old_mode,
            new_mode: self.new_mode,
            is_binary: self.is_binary,
            hunks: self.hunks.hunks,
        }
    }
}

/// The path of a `---`/`+++` line; `None` for `/dev/null`. Git ends the line
/// with a tab when the path has spaces; other tools put a timestamp there.
fn file_header_path(rest: &str, prefix: &str) -> Option<String> {
    let path = match unquote(rest) {
        Some(path) => path,
        None => rest.split('\t').next().unwrap_or(rest).to_string(),
    };
    (path != "/dev/null").then(|| strip_prefix(path, prefix))
}

fn strip_prefix(path: String, prefix: &str) -> String {
    match path.strip_prefix(prefix) {
        Some(rest) => rest.to_string(),
        None => path,
    }
}

/// The two paths of a `diff --git` line (still with their `a/`, `b/`
/// prefixes). Unquoted paths may contain spaces, which makes the split
/// ambiguous; the usual case of equal paths is split in the middle, else at
/// the first ` b/`. Later headers name the paths unambiguously when the file
/// has them.
fn split_git_paths(paths: &str) -> Option<(String, String)> {
    if paths.starts_with('"') {
        let end = quoted_len(paths)?;
        let old = unquote(&paths[..end])?;
        let rest = paths[end..].strip_prefix(' ')?;
        let new = unquote(rest).unwrap_or_else(|| rest.to_string());
        return Some((old, new));
    }
    if let Some(ix) = paths.find(" \"") {
        return Some((paths[..ix].to_string(), unquote(&paths[ix + 1..])?));
    }
    let mid = paths.len() / 2;
    if paths.len() % 2 == 1
        && paths.is_char_boundary(mid)
        && paths.as_bytes()[mid] == b' '
        && paths[..mid].strip_prefix("a/") == paths[mid + 1..].strip_prefix("b/")
    {
        return Some((paths[..mid].to_string(), paths[mid + 1..].to_string()));
    }
    let (old, new) = paths.split_once(" b/")?;
    Some((old.to_string(), format!("b/{new}")))
}

/// Byte length of the C-quoted string `text` starts with, quotes included.
fn quoted_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut ix = 1;
    while ix < bytes.len() {
        match bytes[ix] {
            b'\\' => ix += 2,
            b'"' => return Some(ix + 1),
            _ => ix += 1,
        }
    }
    None
}

/// Decode a path git quoted C-style (`"d\303\244t\ta"`): escapes and octal
/// bytes, which together form UTF-8. `None` unless `text` is exactly one
/// quoted string.
fn unquote(text: &str) -> Option<String> {
    let inner = text.strip_prefix('"')?;
    if quoted_len(text)? != text.len() {
        return None;
    }
    let inner = &inner.as_bytes()[..inner.len() - 1];
    let mut bytes = Vec::with_capacity(inner.len());
    let mut ix = 0;
    while ix < inner.len() {
        let byte = inner[ix];
        ix += 1;
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        let escaped = *inner.get(ix)?;
        ix += 1;
        bytes.push(match escaped {
            b'a' => 0x07,
            b'b' => 0x08,
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => 0x0b,
            b'f' => 0x0c,
            b'r' => b'\r',
            b'0'..=b'7' => {
                let digits = inner.get(ix - 1..ix + 2)?;
                ix += 2;
                u8::from_str_radix(std::str::from_utf8(digits).ok()?, 8).ok()?
            }
            other => other,
        });
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use PatchLineKind::*;

    fn kinds(hunk: &PatchHunk) -> Vec<PatchLineKind> {
        hunk.lines.iter().map(|l| l.kind).collect()
    }

    fn texts(hunk: &PatchHunk) -> Vec<&str> {
        hunk.lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn hunk_header_numbers_and_section() {
        let hunks =
            parse_hunks("@@ -10,4 +10,5 @@ fn main() {\n ctx\n-old\n+new\n+more\n ctx2\n tail\n");
        assert_eq!(hunks.len(), 1);
        let hunk = &hunks[0];
        assert_eq!(
            (
                hunk.old_start,
                hunk.old_lines,
                hunk.new_start,
                hunk.new_lines
            ),
            (10, 4, 10, 5)
        );
        assert_eq!(hunk.section, "fn main() {");
        assert_eq!(
            kinds(hunk),
            [Context, Removed, Added, Added, Context, Context]
        );
        assert_eq!(texts(hunk), ["ctx", "old", "new", "more", "ctx2", "tail"]);
    }

    #[test]
    fn omitted_counts_mean_one_and_no_section_is_empty() {
        let hunks = parse_hunks("@@ -3 +3 @@\n-a\n+b\n");
        assert_eq!(
            (
                hunks[0].old_start,
                hunks[0].old_lines,
                hunks[0].new_start,
                hunks[0].new_lines
            ),
            (3, 1, 3, 1)
        );
        assert_eq!(hunks[0].section, "");
    }

    #[test]
    fn several_hunks() {
        let hunks = parse_hunks("@@ -1,2 +1,2 @@\n-a\n+A\n b\n@@ -20,1 +20,2 @@ impl X\n c\n+d\n");
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[1].new_start, 20);
        assert_eq!(hunks[1].section, "impl X");
        assert_eq!(kinds(&hunks[1]), [Context, Added]);
    }

    #[test]
    fn lines_that_look_like_headers_inside_a_hunk_are_lines() {
        // A removed "-- x" and an added "++ y" print as "--- x" / "+++ y".
        let hunks = parse_hunks("@@ -1,2 +1,2 @@\n--- x\n+++ y\n context\n");
        assert_eq!(kinds(&hunks[0]), [Removed, Added, Context]);
        assert_eq!(texts(&hunks[0]), ["-- x", "++ y", "context"]);
    }

    #[test]
    fn no_newline_marker_flags_the_line_before() {
        let hunks = parse_hunks(
            "@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n",
        );
        let lines = &hunks[0].lines;
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.missing_newline));
        let hunks = parse_hunks("@@ -1,2 +1,2 @@\n a\n-b\n+c\n");
        assert!(hunks[0].lines.iter().all(|l| !l.missing_newline));
    }

    #[test]
    fn crlf_line_endings_are_stripped() {
        let hunks = parse_hunks("@@ -1,2 +1,2 @@ sec\r\n a\r\n-b\r\n+c\r\n");
        assert_eq!(hunks[0].section, "sec");
        assert_eq!(texts(&hunks[0]), ["a", "b", "c"]);
    }

    #[test]
    fn empty_lines_inside_a_hunk_are_empty_context() {
        // Some tools strip the single space of an empty context line.
        let hunks = parse_hunks("@@ -1,3 +1,3 @@\n a\n\n-b\n+c\n");
        assert_eq!(kinds(&hunks[0]), [Context, Context, Removed, Added]);
        assert_eq!(texts(&hunks[0])[1], "");
    }

    #[test]
    fn malformed_input_is_skipped() {
        assert!(parse_hunks("").is_empty());
        assert!(parse_hunks("not a patch\n+x\n").is_empty());
        // A broken header drops its lines; the next good hunk still parses.
        let hunks = parse_hunks("@@ -x,1 +1 @@\n+lost\n@@ -5 +5,2 @@\n a\n+b\n");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_start, 5);
        // Lines beyond the header's counts do not belong to the hunk.
        let hunks = parse_hunks("@@ -1 +1 @@\n-a\n+b\n+extra\n");
        assert_eq!(texts(&hunks[0]), ["a", "b"]);
        // A truncated hunk keeps what is there; unknown prefixes end it.
        let hunks = parse_hunks("@@ -1,5 +1,5 @@\n a\n*junk\n b\n");
        assert_eq!(texts(&hunks[0]), ["a"]);
        // Huge numbers do not overflow.
        assert!(parse_hunks("@@ -99999999999999999999999 +1 @@\n+a\n").is_empty());
    }

    #[test]
    fn text_before_the_first_header_is_ignored() {
        let hunks = parse_hunks("index 123..456\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n");
        assert_eq!(hunks.len(), 1);
        assert_eq!(kinds(&hunks[0]), [Removed, Added]);
    }

    const MULTI: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 83db48f..bf269f4 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@ mod a;
 a
-b
+B
 c
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..e69de29
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+one
+two
diff --git a/gone.txt b/gone.txt
deleted file mode 100755
index e69de29..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
";

    #[test]
    fn multi_file_patch_with_statuses() {
        let files = parse_patch(MULTI);
        assert_eq!(files.len(), 3);

        assert_eq!(files[0].path, "src/lib.rs");
        assert_eq!(files[0].status, ChangeStatus::Modified);
        assert_eq!(files[0].orig_path, None);
        assert_eq!(files[0].hunks.len(), 1);
        assert_eq!(files[0].hunks[0].section, "mod a;");
        assert_eq!(
            kinds(&files[0].hunks[0]),
            [Context, Removed, Added, Context]
        );

        assert_eq!(files[1].path, "new.txt");
        assert_eq!(files[1].status, ChangeStatus::Added);
        assert_eq!(files[1].new_mode.as_deref(), Some("100644"));
        assert_eq!(files[1].hunks[0].old_start, 0);
        assert_eq!(texts(&files[1].hunks[0]), ["one", "two"]);

        assert_eq!(files[2].path, "gone.txt");
        assert_eq!(files[2].status, ChangeStatus::Deleted);
        assert_eq!(files[2].old_mode.as_deref(), Some("100755"));
        assert_eq!(kinds(&files[2].hunks[0]), [Removed]);
        assert!(files.iter().all(|f| !f.is_binary));
    }

    #[test]
    fn renames_and_copies() {
        let files = parse_patch(
            "diff --git a/old/name.rs b/new/name.rs\n\
             similarity index 92%\n\
             rename from old/name.rs\n\
             rename to new/name.rs\n\
             index 1..2 100644\n\
             --- a/old/name.rs\n\
             +++ b/new/name.rs\n\
             @@ -1 +1 @@\n\
             -a\n\
             +b\n\
             diff --git a/x.rs b/y.rs\n\
             similarity index 100%\n\
             rename from x.rs\n\
             rename to y.rs\n\
             diff --git a/c.rs b/d.rs\n\
             similarity index 80%\n\
             copy from c.rs\n\
             copy to d.rs\n",
        );
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "new/name.rs");
        assert_eq!(files[0].orig_path.as_deref(), Some("old/name.rs"));
        assert_eq!(files[0].status, ChangeStatus::Renamed);
        assert_eq!(files[0].similarity, Some(92));
        assert_eq!(files[0].hunks.len(), 1);
        // A pure rename has no hunks.
        assert_eq!(files[1].path, "y.rs");
        assert_eq!(files[1].orig_path.as_deref(), Some("x.rs"));
        assert_eq!(files[1].status, ChangeStatus::Renamed);
        assert_eq!(files[1].similarity, Some(100));
        assert!(files[1].hunks.is_empty());
        assert_eq!(files[2].status, ChangeStatus::Copied);
        assert_eq!(files[2].orig_path.as_deref(), Some("c.rs"));
        assert_eq!(files[2].path, "d.rs");
    }

    #[test]
    fn binary_files_and_mode_changes() {
        let files = parse_patch(
            "diff --git a/img.png b/img.png\n\
             new file mode 100644\n\
             index 0000000..1234567\n\
             Binary files /dev/null and b/img.png differ\n\
             diff --git a/blob.bin b/blob.bin\n\
             index 1..2 100644\n\
             GIT binary patch\n\
             literal 12\n\
             TcmZ?wbhEHbRA*pfU|;|M2XO&j\n\
             \n\
             literal 0\n\
             HcmV?d00001\n\
             \n\
             diff --git a/run.sh b/run.sh\n\
             old mode 100644\n\
             new mode 100755\n",
        );
        assert_eq!(files.len(), 3);
        assert!(files[0].is_binary);
        assert_eq!(files[0].status, ChangeStatus::Added);
        assert!(files[0].hunks.is_empty());
        assert!(files[1].is_binary);
        assert_eq!(files[1].status, ChangeStatus::Modified);
        assert!(files[1].hunks.is_empty());
        assert_eq!(files[2].path, "run.sh");
        assert_eq!(files[2].status, ChangeStatus::Modified);
        assert_eq!(files[2].old_mode.as_deref(), Some("100644"));
        assert_eq!(files[2].new_mode.as_deref(), Some("100755"));
        assert!(!files[2].is_binary);
    }

    #[test]
    fn binary_deletion_without_mode_lines() {
        let files = parse_patch(
            "diff --git a/img.png b/img.png\nBinary files a/img.png and /dev/null differ\n",
        );
        assert_eq!(files[0].status, ChangeStatus::Deleted);
        assert!(files[0].is_binary);
    }

    #[test]
    fn quoted_and_spaced_paths() {
        let files = parse_patch(
            "diff --git \"a/d\\303\\244t a.txt\" \"b/d\\303\\244t a.txt\"\n\
             index 1..2 100644\n\
             --- \"a/d\\303\\244t a.txt\"\n\
             +++ \"b/d\\303\\244t a.txt\"\n\
             @@ -1 +1 @@\n\
             -a\n\
             +b\n\
             diff --git a/with space.txt b/with space.txt\n\
             index 1..2 100644\n\
             --- a/with space.txt\t\n\
             +++ b/with space.txt\t\n\
             @@ -1 +1 @@\n\
             -a\n\
             +b\n\
             diff --git a/tab\\there b/tab\\there\n\
             similarity index 100%\n\
             rename from \"tab\\there\"\n\
             rename to \"tab\\\"quoted\\\"\"\n",
        );
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "dät a.txt");
        assert_eq!(files[1].path, "with space.txt");
        assert_eq!(files[2].orig_path.as_deref(), Some("tab\there"));
        assert_eq!(files[2].path, "tab\"quoted\"");
    }

    #[test]
    fn spaced_paths_without_file_headers_split_at_the_middle() {
        let files = parse_patch("diff --git a/a b/c b/a b/c\nold mode 100644\nnew mode 100755\n");
        assert_eq!(files[0].path, "a b/c");
    }

    #[test]
    fn crlf_patch() {
        let files = parse_patch(&MULTI.replace('\n', "\r\n"));
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].path, "src/lib.rs");
        assert_eq!(texts(&files[0].hunks[0]), ["a", "b", "B", "c"]);
    }

    #[test]
    fn plain_unified_diff_without_git_headers() {
        let files = parse_patch(
            "--- a/one.txt\n+++ b/one.txt\n@@ -1 +1 @@\n-a\n+b\n\
             --- a/two.txt\n+++ b/two.txt\n@@ -1 +1 @@\n-c\n+d\n",
        );
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "one.txt");
        assert_eq!(files[1].path, "two.txt");
        assert_eq!(texts(&files[1].hunks[0]), ["c", "d"]);
    }

    #[test]
    fn garbage_never_panics() {
        assert!(parse_patch("").is_empty());
        assert!(parse_patch("hello\nworld\n").is_empty());
        for text in [
            "diff --git\n",
            "diff --git \"a/unterminated\n",
            "diff --git \"a/\\\n",
            "diff --git a/x b/x\n@@\n@@ -\n@@ -1, +1 @@\n+x\n",
            "diff --git a/x b/x\nsimilarity index 999%\nrename from\n",
            "diff --git a/x b/x\n--- \n+++ \n\\\n",
            "\\ No newline at end of file\n",
        ] {
            let _ = parse_patch(text);
            let _ = parse_hunks(text);
        }
        let files = parse_patch("diff --git a/x b/x\nsimilarity index 999%\n");
        assert_eq!(files[0].similarity, None);
    }
}
