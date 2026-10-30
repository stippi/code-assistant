//! Diff rows and hunks: computed from two texts (a line diff, then a word
//! diff of small replace blocks) or taken from a parsed patch.

use gpui_kit::SharedString;
use similar::{ChangeTag, TextDiff};

/// Normalize text for diff display.
///
/// LLMs frequently emit a spurious leading `\n` at the start of `old_text` or
/// `new_text` JSON string values, and the two sides are not always consistent.
/// Additionally, format-on-save only updates `new_text` (the replace side) while
/// `old_text` keeps the raw LLM value, which can introduce trailing-newline
/// mismatches.
///
/// We strip one leading `\n` (if present) so both sides start at real content,
/// then ensure both end with exactly one `\n` so `TextDiff` with
/// `newline_terminated(true)` treats the last line consistently.  Interior blank
/// lines (intentional insertions) are preserved.
pub fn normalize_for_diff(text: &str) -> String {
    let trimmed = text.strip_prefix('\n').unwrap_or(text);
    let trimmed = trimmed.strip_suffix('\n').unwrap_or(trimmed);
    format!("{trimmed}\n")
}

/// One line of a computed unified diff. `text` is a [`SharedString`] so cached
/// diffs can be re-rendered every frame with cheap clones. `emphasis` marks
/// the byte ranges within `text` that changed *within* the line (word diff);
/// they get a stronger background on top of the row color.
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub tag: ChangeTag,
    pub text: SharedString,
    pub emphasis: Vec<std::ops::Range<usize>>,
}

impl DiffLine {
    /// A row without emphasis; trailing whitespace (and the line ending) is
    /// dropped.
    pub fn plain(tag: ChangeTag, text: &str) -> Self {
        Self {
            tag,
            text: text.trim_end().to_string().into(),
            emphasis: Vec::new(),
        }
    }
}

/// Replace blocks with more lines than this (per side) skip the word-level
/// diff — pairing words across big rewrites produces noise, not signal, and
/// the word diff's cost grows with the block. Same cap as Zed.
const MAX_WORD_DIFF_LINES: usize = 8;

/// Unchanged text between two emphasis ranges with at most this many
/// alphanumeric characters (a space, punctuation, "a", "the", "in") is not
/// worth breaking the emphasis for — the ranges merge across it.
const MAX_MERGED_ANCHOR_CHARS: usize = 3;

/// A line with more emphasis ranges than this (after merging) is a rewrite,
/// not an edit: highlighting would fragment the whole paragraph. The block
/// then shows as plain changes. Counting ranges rather than characters keeps
/// one long insertion, however large, emphasized.
const MAX_EMPHASIS_SPANS: usize = 6;

/// Byte ranges (per side) that a word-level diff of one replace block marks
/// as changed, relative to each side's concatenated block text. Runs our own
/// word diff instead of `similar`'s inline changes: those refuse blocks below
/// a similarity ratio, which drops exactly the case of a few sentences
/// inserted into a short paragraph.
fn word_diff_ranges(
    old: &str,
    new: &str,
) -> (Vec<std::ops::Range<usize>>, Vec<std::ops::Range<usize>>) {
    let diff = TextDiff::configure()
        .algorithm(similar::Algorithm::Patience)
        .diff_unicode_words(old, new);
    let bytes = |slices: &[&str]| slices.iter().map(|s| s.len()).sum::<usize>();
    let (mut old_ranges, mut new_ranges) = (Vec::new(), Vec::new());
    let (mut old_pos, mut new_pos) = (0, 0);
    for op in diff.ops() {
        let old_len = bytes(&diff.old_slices()[op.old_range()]);
        let new_len = bytes(&diff.new_slices()[op.new_range()]);
        if op.tag() != similar::DiffTag::Equal {
            if old_len > 0 {
                old_ranges.push(old_pos..old_pos + old_len);
            }
            if new_len > 0 {
                new_ranges.push(new_pos..new_pos + new_len);
            }
        }
        old_pos += old_len;
        new_pos += new_len;
    }
    (old_ranges, new_ranges)
}

/// Build the [`DiffLine`]s of one side of a replace block from its
/// newline-terminated lines and the block-relative changed ranges. Each
/// line's ranges are clipped to the trimmed line and merged across small
/// anchors.
fn emphasized_lines(
    tag: ChangeTag,
    lines: &[String],
    ranges: &[std::ops::Range<usize>],
    out: &mut Vec<DiffLine>,
) {
    let mut line_start = 0;
    for line in lines {
        let line_end = line_start + line.len();
        let text = line.trim_end();
        let mut emphasis: Vec<std::ops::Range<usize>> = ranges
            .iter()
            .filter(|r| r.start < line_end && r.end > line_start)
            .map(|r| {
                let start = r.start.max(line_start) - line_start;
                let end = (r.end.min(line_end) - line_start).min(text.len());
                start..end
            })
            .filter(|r| r.start < r.end)
            .collect();
        emphasis.dedup_by(|next, prev| {
            let anchor = &text[prev.end..next.start];
            if anchor.chars().filter(|c| c.is_alphanumeric()).count() <= MAX_MERGED_ANCHOR_CHARS {
                prev.end = next.end;
                true
            } else {
                false
            }
        });
        out.push(DiffLine {
            tag,
            text: text.to_string().into(),
            emphasis,
        });
        line_start = line_end;
    }
}

/// The rows of one change block — `old` lines removed, `new` lines added in
/// their place — appended to `out`, deletions first. A small block with both
/// sides gets word-level emphasis. Lines may or may not end in a newline.
pub fn push_replace_block(old: &[&str], new: &[&str], out: &mut Vec<DiffLine>) {
    if old.is_empty() || new.is_empty() || old.len().max(new.len()) > MAX_WORD_DIFF_LINES {
        out.extend(old.iter().map(|l| DiffLine::plain(ChangeTag::Delete, l)));
        out.extend(new.iter().map(|l| DiffLine::plain(ChangeTag::Insert, l)));
        return;
    }
    let terminated = |lines: &[&str]| -> Vec<String> {
        lines
            .iter()
            .map(|l| format!("{}\n", l.strip_suffix('\n').unwrap_or(l)))
            .collect()
    };
    let (old, new) = (terminated(old), terminated(new));
    let (old_ranges, new_ranges) = word_diff_ranges(&old.concat(), &new.concat());
    let start = out.len();
    emphasized_lines(ChangeTag::Delete, &old, &old_ranges, out);
    emphasized_lines(ChangeTag::Insert, &new, &new_ranges, out);
    // The word diff pairs both sides of the block, so the rewrite verdict
    // covers both: emphasis on one side with none on the other would
    // suggest a deletion without a counterpart.
    let rewritten = out[start..]
        .iter()
        .any(|l| l.emphasis.len() > MAX_EMPHASIS_SPANS);
    if rewritten {
        for line in &mut out[start..] {
            line.emphasis.clear();
        }
    }
}

/// Expand one diff op into [`DiffLine`]s.
fn collect_change_lines<'a>(
    diff: &'a TextDiff<'a, 'a, 'a, str>,
    op: &similar::DiffOp,
    out: &mut Vec<DiffLine>,
) {
    if op.tag() == similar::DiffTag::Replace {
        push_replace_block(
            &diff.old_slices()[op.old_range()],
            &diff.new_slices()[op.new_range()],
            out,
        );
    } else {
        for change in diff.iter_changes(op) {
            out.push(DiffLine::plain(change.tag(), change.value()));
        }
    }
}

/// Run the line diff (the expensive part: normalization + Myers diff + per-line
/// allocations). Callers that render on every frame should call this once per
/// content change and cache the result.
pub fn compute_diff_lines(old_text: &str, new_text: &str) -> Vec<DiffLine> {
    let old_norm = normalize_for_diff(old_text);
    let new_norm = normalize_for_diff(new_text);

    let diff = TextDiff::configure()
        .newline_terminated(true)
        .diff_lines(&old_norm, &new_norm);

    let mut lines = Vec::new();
    for op in diff.ops() {
        collect_change_lines(&diff, op, &mut lines);
    }
    lines
}

/// One hunk of a unified diff: a run of changed lines plus surrounding
/// context, positioned at `old_start` / `new_start` (1-based) in the old and
/// new file.
#[derive(Debug, Clone)]
pub struct DiffHunk {
    pub old_start: usize,
    pub new_start: usize,
    /// The section text of a patch's `@@` header (usually the enclosing
    /// function); computed diffs have none.
    pub header: Option<SharedString>,
    pub lines: Vec<DiffLine>,
}

/// Like [`compute_diff_lines`], but grouped into hunks with `context` lines
/// of surrounding context (à la `git diff`) — unchanged stretches between
/// hunks are dropped entirely, which keeps the element count proportional to
/// the *changed* lines instead of the file size.
pub fn compute_diff_hunks(old_text: &str, new_text: &str, context: usize) -> Vec<DiffHunk> {
    let old_norm = normalize_for_diff(old_text);
    let new_norm = normalize_for_diff(new_text);

    let diff = TextDiff::configure()
        .newline_terminated(true)
        .diff_lines(&old_norm, &new_norm);

    diff.grouped_ops(context)
        .iter()
        .map(|ops| {
            let mut lines = Vec::new();
            for op in ops {
                collect_change_lines(&diff, op, &mut lines);
            }
            DiffHunk {
                old_start: ops.first().map(|op| op.old_range().start + 1).unwrap_or(1),
                new_start: ops.first().map(|op| op.new_range().start + 1).unwrap_or(1),
                header: None,
                lines,
            }
        })
        .collect()
}

/// A whole file as one one-sided hunk (pure add or pure delete). No diff
/// computation — diffing against an empty side would only produce a phantom
/// deleted/inserted blank line (`normalize_for_diff` maps "" to "\n").
pub fn single_sided_hunk(text: &str, tag: ChangeTag) -> Vec<DiffHunk> {
    let lines: Vec<DiffLine> = normalize_for_diff(text)
        .lines()
        .map(|l| DiffLine::plain(tag, l))
        .collect();
    if lines.is_empty() {
        return Vec::new();
    }
    vec![DiffHunk {
        old_start: 1,
        new_start: 1,
        header: None,
        lines,
    }]
}

/// The hunks of a parsed patch. Each run of removed lines followed by added
/// lines is a change block and gets word emphasis like a computed diff.
pub fn hunks_from_patch(hunks: &[git::PatchHunk]) -> Vec<DiffHunk> {
    use git::PatchLineKind;
    hunks
        .iter()
        .map(|hunk| {
            let mut lines = Vec::with_capacity(hunk.lines.len());
            let (mut removed, mut added): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
            for line in &hunk.lines {
                match line.kind {
                    PatchLineKind::Added => added.push(&line.text),
                    PatchLineKind::Removed if added.is_empty() => removed.push(&line.text),
                    kind => {
                        push_replace_block(&removed, &added, &mut lines);
                        (removed, added) = (Vec::new(), Vec::new());
                        match kind {
                            PatchLineKind::Removed => removed.push(&line.text),
                            _ => lines.push(DiffLine::plain(ChangeTag::Equal, &line.text)),
                        }
                    }
                }
            }
            push_replace_block(&removed, &added, &mut lines);
            DiffHunk {
                old_start: hunk.old_start,
                new_start: hunk.new_start,
                header: (!hunk.section.is_empty()).then(|| hunk.section.clone().into()),
                lines,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_diff_hunks_groups_changes_with_context() {
        let old: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        let mut new_lines: Vec<String> = (1..=20).map(|i| format!("line {i}\n")).collect();
        new_lines[2] = "changed 3\n".into();
        new_lines[15] = "changed 16\n".into();
        let new: String = new_lines.concat();

        let hunks = compute_diff_hunks(&old, &new, 3);
        assert_eq!(hunks.len(), 2, "two distant changes → two hunks");
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (1, 1));
        assert_eq!((hunks[1].old_start, hunks[1].new_start), (13, 13));
        for hunk in &hunks {
            let count = |tag| hunk.lines.iter().filter(|l| l.tag == tag).count();
            assert_eq!((count(ChangeTag::Delete), count(ChangeTag::Insert)), (1, 1));
            assert!(
                count(ChangeTag::Equal) <= 6,
                "at most 3 context lines per side"
            );
            assert!(hunk.header.is_none());
        }
    }

    #[test]
    fn compute_diff_lines_marks_word_level_changes() {
        let lines = compute_diff_lines("fn foo(alpha: u32) {}\n", "fn foo(beta: u32) {}\n");
        let del = lines.iter().find(|l| l.tag == ChangeTag::Delete).unwrap();
        let ins = lines.iter().find(|l| l.tag == ChangeTag::Insert).unwrap();

        // The changed identifier is emphasized — not the whole line.
        assert_eq!(del.emphasis.len(), 1);
        assert_eq!(&del.text[del.emphasis[0].clone()], "alpha");
        assert_eq!(ins.emphasis.len(), 1);
        assert_eq!(&ins.text[ins.emphasis[0].clone()], "beta");

        // Unchanged context lines carry no emphasis.
        assert!(
            lines
                .iter()
                .filter(|l| l.tag == ChangeTag::Equal)
                .all(|l| l.emphasis.is_empty())
        );
    }

    #[test]
    fn word_diff_merges_ranges_split_only_by_whitespace() {
        let lines = compute_diff_lines("keep foo bar keep\n", "keep qux quux keep\n");
        let ins = lines.iter().find(|l| l.tag == ChangeTag::Insert).unwrap();
        // "qux" and "quux" are separate word tokens with an unchanged space
        // between them; visually that is one change.
        assert_eq!(ins.emphasis.len(), 1);
        assert_eq!(&ins.text[ins.emphasis[0].clone()], "qux quux");
    }

    #[test]
    fn word_diff_kept_for_small_edit_in_long_paragraph() {
        // A long prose paragraph (one line) with a single changed word is
        // exactly where word emphasis helps most — length must not disable it.
        let filler = "the data center deployment ".repeat(25);
        let old = format!("{filler}serving a jurisdiction.\n");
        let new = format!("{filler}serving one or more jurisdictions.\n");
        assert!(old.len() > 512);
        let lines = compute_diff_lines(&old, &new);
        let ins = lines.iter().find(|l| l.tag == ChangeTag::Insert).unwrap();
        assert_eq!(ins.emphasis.len(), 1);
        assert_eq!(
            &ins.text[ins.emphasis[0].clone()],
            "one or more jurisdictions"
        );
    }

    #[test]
    fn word_diff_keeps_both_sides_of_a_partially_rewritten_paragraph() {
        // Deleted and inserted passages both get emphasis; small anchors
        // ("service", "in") do not fragment it. The counterpart of a
        // highlighted deletion must be visible on the inserted side.
        let lines = compute_diff_lines(
            "- **Main Tenant** — AI Core's top-level tenant, mapped one-to-one to a service instance in a BTP subaccount.\n",
            "- **Main Tenant** — AI Core's top-level tenant, identified by the BTP subaccount / zone ID. Multiple AI Core service instances in one subaccount reference the same Main Tenant and Resource Groups.\n",
        );
        let del = lines.iter().find(|l| l.tag == ChangeTag::Delete).unwrap();
        let ins = lines.iter().find(|l| l.tag == ChangeTag::Insert).unwrap();
        assert!(!del.emphasis.is_empty());
        assert!(!ins.emphasis.is_empty());
        assert!(ins.emphasis.len() <= 3, "{:?}", ins.emphasis);
        assert!(&ins.text[ins.emphasis[0].clone()].starts_with("identified by the BTP subaccount"));
    }

    #[test]
    fn word_diff_keeps_long_insertion_into_short_paragraph() {
        // Two short sentences become five: the insertion dwarfs the original,
        // yet it is one contiguous change and must be emphasized as such.
        let lines = compute_diff_lines(
            "Traces stay local. Keys stay local.\n",
            "Traces stay local. Every trace is written by the consumer that received it, and that consumer runs in the same jurisdiction as the deployment. Retention follows the jurisdiction's own schedule rather than a global default. Keys stay local.\n",
        );
        let ins = lines.iter().find(|l| l.tag == ChangeTag::Insert).unwrap();
        assert_eq!(ins.emphasis.len(), 1, "{:?}", ins.emphasis);
        assert!(&ins.text[ins.emphasis[0].clone()].starts_with("Every trace"));
    }

    #[test]
    fn word_diff_dropped_for_fragmented_rewrite() {
        // Every other word changed, with real words as anchors in between:
        // more emphasis spans than a reader can follow, so both sides fall
        // back to plain changes.
        let lines = compute_diff_lines(
            "alpha keep1 beta keep2 gamma keep3 delta keep4 epsilon keep5 zeta keep6 eta keep7 theta\n",
            "one keep1 two keep2 three keep3 four keep4 five keep5 six keep6 seven keep7 eight\n",
        );
        assert!(
            lines
                .iter()
                .filter(|l| l.tag != ChangeTag::Equal)
                .all(|l| l.emphasis.is_empty())
        );
    }

    #[test]
    fn replace_block_with_unterminated_lines_gets_emphasis() {
        let mut out = Vec::new();
        push_replace_block(&["let a = 1;", "keep"], &["let a = 2;", "keep"], &mut out);
        let tags: Vec<_> = out.iter().map(|l| l.tag).collect();
        use ChangeTag::*;
        assert_eq!(tags, [Delete, Delete, Insert, Insert]);
        assert_eq!(&out[0].text[out[0].emphasis[0].clone()], "1");
        assert_eq!(&out[2].text[out[2].emphasis[0].clone()], "2");
        assert!(out[1].emphasis.is_empty());

        // One-sided and oversized blocks stay plain.
        let mut out = Vec::new();
        push_replace_block(&[], &["new"], &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0].emphasis.is_empty());
        let many = vec!["x = 1"; MAX_WORD_DIFF_LINES + 1];
        let mut out = Vec::new();
        push_replace_block(&many, &["x = 2"], &mut out);
        assert!(out.iter().all(|l| l.emphasis.is_empty()));
    }

    #[test]
    fn single_sided_hunk_is_one_pure_hunk() {
        let hunks = single_sided_hunk("a\nb\nc\n", ChangeTag::Insert);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].new_start, 1);
        assert!(hunks[0].lines.iter().all(|l| l.tag == ChangeTag::Insert));
        assert_eq!(hunks[0].lines.len(), 3);
    }

    #[test]
    fn patch_hunks_keep_positions_header_and_order() {
        let patch = git::parse_hunks(
            "@@ -10,5 +10,5 @@ fn main() {\n ctx\n-let a = 1;\n+let a = 2;\n ctx\n+added\n-removed\n tail  \n\
             @@ -40 +40,2 @@\n x\n+y\n",
        );
        let hunks = hunks_from_patch(&patch);
        assert_eq!(hunks.len(), 2);
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (10, 10));
        assert_eq!(hunks[0].header.as_deref(), Some("fn main() {"));
        assert_eq!(hunks[1].header, None);
        use ChangeTag::*;
        let tags: Vec<_> = hunks[0].lines.iter().map(|l| l.tag).collect();
        // An added line followed by a removed one keeps the patch's order.
        assert_eq!(tags, [Equal, Delete, Insert, Equal, Insert, Delete, Equal]);
        let texts: Vec<_> = hunks[0].lines.iter().map(|l| l.text.as_ref()).collect();
        assert_eq!(texts[6], "tail");
        // The change block is word-diffed.
        assert_eq!(
            &hunks[0].lines[2].text[hunks[0].lines[2].emphasis[0].clone()],
            "2"
        );
        // "+added" then "-removed" are not one block: no emphasis.
        assert!(hunks[0].lines[4].emphasis.is_empty());
    }

    /// Helper: compute diff tags after the same normalization the line diff uses.
    fn diff_tags(old: &str, new: &str) -> Vec<(ChangeTag, String)> {
        let old_norm = normalize_for_diff(old);
        let new_norm = normalize_for_diff(new);
        let diff = TextDiff::configure()
            .newline_terminated(true)
            .diff_lines(&old_norm, &new_norm);
        diff.iter_all_changes()
            .map(|c| (c.tag(), c.value().trim_end().to_string()))
            .collect()
    }

    #[test]
    fn test_diff_no_spurious_leading_green_line() {
        // LLMs sometimes emit new_text with a leading \n that old_text lacks.
        // The normalization should prevent a spurious "added empty line" at top.
        let tags = diff_tags(
            "/// comment\n#[derive(Debug)]",
            "\n/// comment\n#[derive(Debug)]\npub new_field: u32,",
        );
        // First line should be Equal, not Insert
        assert_eq!(
            tags[0].0,
            ChangeTag::Equal,
            "first line should be equal, got {:?}",
            tags
        );
        assert_eq!(tags[0].1, "/// comment");
        // The actually new line should be Insert
        assert!(
            tags.iter()
                .any(|(t, l)| *t == ChangeTag::Insert && l == "pub new_field: u32,")
        );
    }

    #[test]
    fn test_diff_trailing_newline_mismatch_no_spurious_change() {
        // old has no trailing \n, new does — should not produce a spurious change
        let tags = diff_tags("line1\nline2", "line1\nline2\n");
        assert!(
            tags.iter().all(|(t, _)| *t == ChangeTag::Equal),
            "trailing newline mismatch should not produce changes: {:?}",
            tags
        );
    }

    #[test]
    fn test_diff_normal_addition() {
        let tags = diff_tags("line1\nline2\n", "line1\nline2\nnew_line\n");
        assert_eq!(tags.len(), 3);
        assert_eq!(tags[0], (ChangeTag::Equal, "line1".to_string()));
        assert_eq!(tags[1], (ChangeTag::Equal, "line2".to_string()));
        assert_eq!(tags[2], (ChangeTag::Insert, "new_line".to_string()));
    }

    #[test]
    fn test_diff_intentional_blank_line_insertion_preserved() {
        // Adding a blank line between two functions is a real change that must be shown.
        let tags = diff_tags("fn a() {}\nfn b() {}", "fn a() {}\n\nfn b() {}");
        let inserts: Vec<_> = tags
            .iter()
            .filter(|(t, _)| *t == ChangeTag::Insert)
            .collect();
        assert_eq!(inserts.len(), 1, "should insert one blank line: {:?}", tags);
        assert_eq!(inserts[0].1, "", "the inserted line should be blank");
    }

    #[test]
    fn test_diff_leading_newline_mismatch_both_directions() {
        // old has leading \n, new doesn't — should not produce spurious change
        let tags = diff_tags("\nline1\nline2", "line1\nline2\nnew");
        assert_eq!(
            tags[0].0,
            ChangeTag::Equal,
            "first line should be equal: {:?}",
            tags
        );
        assert_eq!(tags[0].1, "line1");
    }
}
