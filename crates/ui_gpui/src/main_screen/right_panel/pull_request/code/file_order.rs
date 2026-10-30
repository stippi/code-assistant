//! The order the Code tab lists files in, ported from T3 Code's
//! `orderDiffFiles` and `orderDiffFilesLikeTree`: within a diff page,
//! sources before tests before generated files ([`Tier`]), a source after
//! the sources it imports, tests in the order of the sources they cover;
//! then all pages regrouped so each directory stays together, as a file
//! tree would list them.
//!
//! Imports are read from the diff's lines with the patterns T3 knows
//! (`from "…"`, `import "…"`, `require("…")`); Rust's `use` / `mod` are not
//! resolved, so Rust sources keep path order.

use super::file_kind::{Tier, tier};
use std::collections::{HashMap, HashSet};

const MODULE_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"];

/// The order of one page's files: indices into `paths`. `lines` yields the
/// diff lines of the file at an index.
pub fn order_files<'a, L>(paths: &[&str], lines: impl Fn(usize) -> L) -> Vec<usize>
where
    L: IntoIterator<Item = &'a str>,
{
    let tiers: Vec<Tier> = paths.iter().map(|p| tier(p)).collect();
    let tiers = &tiers;
    let of_tier = |t: Tier| (0..paths.len()).filter(move |&ix| tiers[ix] == t);

    let sources: Vec<usize> = of_tier(Tier::Source).collect();
    let mut by_module: HashMap<&str, usize> = HashMap::new();
    let mut by_base: HashMap<&str, usize> = HashMap::new();
    let mut ambiguous: HashSet<&str> = HashSet::new();
    for &ix in &sources {
        by_module.insert(strip_extension(paths[ix]), ix);
        let base = base_name(paths[ix]);
        if by_base.insert(base, ix).is_some() {
            ambiguous.insert(base);
        }
    }
    by_base.retain(|base, _| !ambiguous.contains(base));

    let imports: HashMap<usize, HashSet<usize>> = sources
        .iter()
        .map(|&ix| {
            let imported = lines(ix)
                .into_iter()
                .flat_map(import_specifiers)
                .filter_map(|spec| resolve(paths[ix], spec, &by_module, &by_base))
                .filter(|&target| target != ix)
                .collect();
            (ix, imported)
        })
        .collect();
    let ordered_sources = order_by_imports(&sources, paths, &imports);

    let mut source_positions: HashMap<&str, usize> = HashMap::new();
    for (position, &ix) in ordered_sources.iter().enumerate() {
        source_positions
            .entry(base_name(paths[ix]))
            .or_insert(position);
    }
    let mut tests: Vec<usize> = of_tier(Tier::Test).collect();
    tests.sort_by(|&a, &b| {
        let position = |ix: usize| {
            source_positions
                .get(tested_base_name(paths[ix]).as_str())
                .copied()
                .unwrap_or(usize::MAX)
        };
        position(a)
            .cmp(&position(b))
            .then_with(|| paths[a].cmp(paths[b]))
    });
    let mut generated: Vec<usize> = of_tier(Tier::Generated).collect();
    generated.sort_by(|&a, &b| paths[a].cmp(paths[b]));

    ordered_sources
        .into_iter()
        .chain(tests)
        .chain(generated)
        .collect()
}

/// `paths` regrouped so each directory stays contiguous, in the order its
/// first file had: indices into `paths`.
pub fn order_like_tree(paths: &[&str]) -> Vec<usize> {
    let mut positions: HashMap<&str, usize> = HashMap::new();
    for (ix, path) in paths.iter().enumerate() {
        positions.insert(path, ix);
        for (end, _) in path.match_indices('/') {
            positions.entry(&path[..=end]).or_insert(ix);
        }
    }
    let keys: Vec<Vec<usize>> = paths
        .iter()
        .map(|path| {
            path.match_indices('/')
                .map(|(end, _)| positions[&path[..=end]])
                .chain([positions[path]])
                .collect()
        })
        .collect();
    let mut order: Vec<usize> = (0..paths.len()).collect();
    // Lexicographic on the keys, a shorter key (a file beside a directory
    // that started at the same position) first.
    order.sort_by(|&a, &b| keys[a].cmp(&keys[b]));
    order
}

/// Picks, in path order, the first file whose imports are all placed; in a
/// cycle the first remaining file.
fn order_by_imports(
    files: &[usize],
    paths: &[&str],
    imports: &HashMap<usize, HashSet<usize>>,
) -> Vec<usize> {
    let mut sorted = files.to_vec();
    sorted.sort_by(|&a, &b| paths[a].cmp(paths[b]));
    let mut remaining: HashSet<usize> = files.iter().copied().collect();
    let mut ordered = Vec::with_capacity(files.len());
    while !remaining.is_empty() {
        let mut candidates = sorted.iter().copied().filter(|ix| remaining.contains(ix));
        let first = candidates.clone().next();
        let next = candidates
            .find(|ix| imports[ix].iter().all(|dep| !remaining.contains(dep)))
            .or(first)
            .expect("a file remains");
        remaining.remove(&next);
        ordered.push(next);
    }
    ordered
}

/// The file a specifier names among the page's sources: by module path
/// (with or without `/index`), else by its last segment when that is
/// unambiguous.
fn resolve(
    from: &str,
    specifier: &str,
    by_module: &HashMap<&str, usize>,
    by_base: &HashMap<&str, usize>,
) -> Option<usize> {
    let path = if specifier.starts_with('.') {
        resolve_relative(from, specifier)
    } else {
        specifier.to_owned()
    };
    // Module keys omit extensions, which can differ from disk (`.js` for `.ts`).
    let module = MODULE_EXTENSIONS
        .iter()
        .find_map(|ext| path.strip_suffix(ext))
        .unwrap_or(&path);
    by_module
        .get(module)
        .or_else(|| by_module.get(format!("{module}/index").as_str()))
        .or_else(|| by_base.get(base_name(module)))
        .copied()
}

fn resolve_relative(from: &str, specifier: &str) -> String {
    let mut segments: Vec<&str> = from.split('/').collect();
    segments.pop();
    for part in specifier.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                segments.pop();
            }
            part => segments.push(part),
        }
    }
    segments.join("/")
}

/// The quoted specifiers of `from "…"`, `import "…"` and `require("…")` in
/// one line (T3's `IMPORT_SPECIFIER`).
fn import_specifiers(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let skip_space = |mut i: usize| {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    let mut specifiers = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let keyword = [&b"from"[..], b"import", b"require"]
            .into_iter()
            .find(|k| bytes[i..].starts_with(k));
        let found = keyword
            .filter(|_| i == 0 || !is_word(bytes[i - 1]))
            .and_then(|keyword| {
                let mut j = skip_space(i + keyword.len());
                if keyword == b"require" {
                    if bytes.get(j) != Some(&b'(') {
                        return None;
                    }
                    j = skip_space(j + 1);
                }
                if bytes.get(j) == Some(&b'(') {
                    j += 1;
                }
                j = skip_space(j);
                if !matches!(bytes.get(j), Some(b'"' | b'\'')) {
                    return None;
                }
                let start = j + 1;
                let len = bytes[start..]
                    .iter()
                    .position(|&b| b == b'"' || b == b'\'')?;
                (len > 0).then_some((start, start + len))
            });
        match found {
            Some((start, end)) => {
                specifiers.push(&line[start..end]);
                i = end + 1;
            }
            None => i += 1,
        }
    }
    specifiers
}

fn strip_extension(path: &str) -> &str {
    match (path.rfind('.'), path.rfind('/')) {
        (Some(dot), Some(slash)) if dot > slash => &path[..dot],
        (Some(dot), None) => &path[..dot],
        _ => path,
    }
}

fn base_name(path: &str) -> &str {
    strip_extension(path.rsplit('/').next().unwrap_or_default())
}

/// The base name of the source a test covers: `a` for `a.test.ts` and for
/// `__tests__/a.ts`.
fn tested_base_name(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or_default();
    let name = [".test.", ".spec."]
        .iter()
        .filter_map(|marker| name.find(marker))
        .min()
        .map_or(name, |at| &name[..at]);
    name.rsplit_once('.')
        .map_or(name, |(stem, _)| stem)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Order `files` (path, added lines) and return the paths.
    fn order(files: &[(&str, &[&str])]) -> Vec<String> {
        let paths: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
        order_files(&paths, |ix| files[ix].1.iter().copied())
            .into_iter()
            .map(|ix| paths[ix].to_owned())
            .collect()
    }

    fn plain<'a>(paths: &[&'a str]) -> Vec<(&'a str, &'static [&'static str])> {
        paths.iter().map(|p| (*p, &[][..])).collect()
    }

    #[test]
    fn sources_come_before_tests_and_generated_files() {
        let source = ["src/app.ts", "src/dist.ts", "src/testing.ts"];
        let tests = [
            "src/__tests__/app.ts",
            "src/app.spec.tsx",
            "src/app.test.ts",
            "test/app.ts",
            "tests/helpers/app.ts",
        ];
        let generated = [
            "apps/web/package-lock.json",
            "dist/app.js",
            "packages/core/vendor/lib.js",
            "pnpm-lock.yaml",
            "public/app.min.js",
            "src/__snapshots__/app.ts",
            "src/api.generated.ts",
            "src/app.test.ts.snap",
        ];
        let mut all: Vec<&str> = [&generated[..], &tests, &source].concat();
        all.reverse();
        let expected: Vec<&str> = [&source[..], &tests, &generated].concat();
        assert_eq!(order(&plain(&all)), expected);
    }

    #[test]
    fn imports_resolve_relative_index_extension_and_last_segment() {
        assert_eq!(
            order(&[
                ("src/ui/a.ts", &[r#"import { b } from "../lib/b";"#]),
                ("src/lib/b.ts", &[])
            ]),
            ["src/lib/b.ts", "src/ui/a.ts"]
        );
        assert_eq!(
            order(&[
                ("src/a.ts", &[r#"import { b } from "./lib";"#]),
                ("src/lib/index.ts", &[])
            ]),
            ["src/lib/index.ts", "src/a.ts"]
        );
        assert_eq!(
            order(&[
                ("src/a.ts", &[r#"import { b } from "~/lib/b";"#]),
                ("lib/b.ts", &[])
            ]),
            ["lib/b.ts", "src/a.ts"]
        );
        assert_eq!(
            order(&[
                ("src/a.ts", &[r#"import { b } from "./b.js";"#]),
                ("src/b.ts", &[])
            ]),
            ["src/b.ts", "src/a.ts"]
        );
    }

    #[test]
    fn an_ambiguous_last_segment_is_not_guessed() {
        assert_eq!(
            order(&[
                ("src/a.ts", &[r#"import { b } from "~/somewhere/b";"#]),
                ("one/b.ts", &[]),
                ("two/b.ts", &[]),
            ]),
            ["one/b.ts", "src/a.ts", "two/b.ts"]
        );
    }

    #[test]
    fn require_and_bare_imports_count_and_cycles_keep_path_order() {
        assert_eq!(
            order(&[
                (
                    "src/a.ts",
                    &[r#"const b = require("./b");"#, r#"import "./c";"#]
                ),
                ("src/b.ts", &[]),
                ("src/c.ts", &[]),
            ]),
            ["src/b.ts", "src/c.ts", "src/a.ts"]
        );
        assert_eq!(
            order(&[
                ("src/b.ts", &[r#"import { a } from "./a";"#]),
                ("src/a.ts", &[r#"import { b } from "./b";"#]),
            ]),
            ["src/a.ts", "src/b.ts"]
        );
    }

    #[test]
    fn tests_follow_the_sources_they_cover() {
        assert_eq!(
            order(&[
                ("pnpm-lock.yaml", &[]),
                ("src/a.test.ts", &[]),
                ("src/__tests__/b.ts", &[]),
                ("src/a.ts", &[r#"import { b } from "./b";"#]),
                ("src/b.ts", &[]),
                ("dist/a.js", &[]),
            ]),
            [
                "src/b.ts",
                "src/a.ts",
                "src/__tests__/b.ts",
                "src/a.test.ts",
                "dist/a.js",
                "pnpm-lock.yaml",
            ]
        );
        // A test whose source is not in the diff comes last.
        assert_eq!(
            order(&plain(&["src/orphan.spec.ts", "src/a.test.ts", "src/a.ts"])),
            ["src/a.ts", "src/a.test.ts", "src/orphan.spec.ts"]
        );
    }

    #[test]
    fn specifiers_are_read_like_t3s_pattern() {
        assert_eq!(
            import_specifiers(r#"import x from 'a'; export * from "b""#),
            ["a", "b"]
        );
        assert_eq!(import_specifiers(r#"await import("./lazy")"#), ["./lazy"]);
        assert!(import_specifiers(r#"transform("x"); fromage "y""#).is_empty());
        assert!(import_specifiers(r#"from """#).is_empty());
        assert!(import_specifiers("grüße from ünïcode").is_empty());
    }

    #[test]
    fn tree_order_keeps_directories_together() {
        let paths = [
            "src/b.ts",
            "lib/x.ts",
            "src/a.ts",
            "README.md",
            "src/ui/c.ts",
        ];
        let ordered: Vec<&str> = order_like_tree(&paths)
            .into_iter()
            .map(|ix| paths[ix])
            .collect();
        assert_eq!(
            ordered,
            [
                "src/b.ts",
                "src/a.ts",
                "src/ui/c.ts",
                "lib/x.ts",
                "README.md"
            ]
        );
    }
}
