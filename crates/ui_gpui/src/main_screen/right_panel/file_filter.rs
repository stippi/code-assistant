//! Fuzzy matching of project paths for the Files view's filter field.
//!
//! A query matches a path when its characters appear in order (ignoring
//! case). Matches in the file name, at word starts and in runs score higher,
//! so `svc` finds `session/service.rs` before `src/vendor/c.rs`.

/// One matching path and the byte offsets of the matched characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileMatch {
    pub path: String,
    pub positions: Vec<usize>,
    score: i64,
}

/// The best `limit` matches of `query` among `paths`, best first. Spaces in
/// the query are ignored.
pub(crate) fn filter_paths(paths: &[String], query: &str, limit: usize) -> Vec<FileMatch> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if query.is_empty() {
        return Vec::new();
    }
    let mut matches: Vec<FileMatch> = paths
        .iter()
        .filter_map(|path| match_path(path, &query))
        .collect();
    matches.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.path.len().cmp(&b.path.len()))
            .then_with(|| a.path.cmp(&b.path))
    });
    matches.truncate(limit);
    matches
}

fn match_path(path: &str, query: &[char]) -> Option<FileMatch> {
    let name_start = path.rfind('/').map_or(0, |i| i + 1);
    // Prefer matching the query's tail in the file name: walk backwards so
    // each query character takes the rightmost possible position.
    let chars: Vec<(usize, char)> = path.char_indices().collect();
    let mut positions = Vec::with_capacity(query.len());
    let mut qi = query.len();
    for &(offset, c) in chars.iter().rev() {
        if qi == 0 {
            break;
        }
        if c.to_lowercase().eq(std::iter::once(query[qi - 1])) {
            positions.push(offset);
            qi -= 1;
        }
    }
    if qi > 0 {
        return None;
    }
    positions.reverse();

    let mut score = 0i64;
    let mut previous: Option<usize> = None;
    for &offset in &positions {
        score += 1;
        if offset >= name_start {
            score += 2;
        }
        let at_word_start = offset == 0
            || path[..offset]
                .chars()
                .next_back()
                .is_some_and(|p| matches!(p, '/' | '_' | '-' | '.' | ' '));
        if at_word_start {
            score += 3;
        }
        if let Some(prev) = previous {
            let gap = path[prev..offset].chars().count().saturating_sub(1);
            if gap == 0 {
                score += 4;
            } else {
                score -= gap.min(8) as i64 / 2;
            }
        }
        previous = Some(offset);
    }
    Some(FileMatch {
        path: path.to_owned(),
        positions,
        score,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn matches_in_order_ignoring_case() {
        let all = paths(&["src/Lib.rs", "README.md", "src/main.rs"]);
        let found: Vec<_> = filter_paths(&all, "LIB", 10)
            .into_iter()
            .map(|m| m.path)
            .collect();
        assert_eq!(found, vec!["src/Lib.rs"]);
        assert!(filter_paths(&all, "zz", 10).is_empty());
        assert!(filter_paths(&all, "  ", 10).is_empty());
    }

    #[test]
    fn prefers_file_names_and_runs() {
        let all = paths(&[
            "src/vendor/c.rs",
            "crates/core/src/session/service.rs",
            "docs/service-notes.md",
        ]);
        let found: Vec<_> = filter_paths(&all, "service.rs", 10)
            .into_iter()
            .map(|m| m.path)
            .collect();
        assert_eq!(found[0], "crates/core/src/session/service.rs");
    }

    #[test]
    fn reports_matched_positions_and_limits_results() {
        let all = paths(&["a/b.rs", "a/bb.rs", "a/bbb.rs"]);
        let found = filter_paths(&all, "b.rs", 2);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, "a/b.rs");
        assert_eq!(found[0].positions, vec![2, 3, 4, 5]);
    }
}
