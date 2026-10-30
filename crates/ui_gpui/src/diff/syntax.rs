//! Syntax highlighting for diff rows. Both sides of a diff are parsed once
//! (tree-sitter, via gpui-component's [`SyntaxHighlighter`]) on a background
//! thread, and the styles of every line are computed once per theme, ideally
//! right there ([`DiffSyntax::prime`]): a tree-sitter query per row and frame
//! showed up as up to 7% of a frame's draw time. Rendering then only slices
//! the cached line styles.
//!
//! A side is either one text (a whole file, or a tool card's snippet) or,
//! for a patch, which has no whole files, one text per hunk
//! ([`SyntaxOrigin`]). Hunks are parsed on their own, so a construct cut off
//! at one hunk's end cannot color the next, but share one highlighter per
//! side: building one compiles the grammar's queries, the costly part.

use super::model::DiffHunk;
use gpui_kit::HighlightStyle;
use gpui_kit::component::Rope;
use gpui_kit::component::ThemeMode;
use gpui_kit::component::highlighter::{
    HighlightTheme, Language, LanguageRegistry, SyntaxHighlighter,
};
use similar::ChangeTag;
use std::ops::Range;
use std::sync::Mutex;

/// Styles of one line, as ranges relative to the line's start.
type LineStyles = Vec<(Range<usize>, HighlightStyle)>;

/// Identifies the theme a [`StyleCache`] was computed for. The address alone
/// could be reused by a later theme, so name and appearance are compared too.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ThemeKey {
    address: usize,
    name: String,
    appearance: ThemeMode,
}

impl ThemeKey {
    fn of(theme: &HighlightTheme) -> Self {
        Self {
            address: theme as *const HighlightTheme as usize,
            name: theme.name.clone(),
            appearance: theme.appearance,
        }
    }
}

/// The styles of every line of one side, per segment, for one theme.
struct StyleCache {
    theme: ThemeKey,
    segments: Vec<Vec<LineStyles>>,
}

/// The grammar name for a file, if gpui-component ships one for it.
pub fn language_for_path(path: &str) -> Option<&'static str> {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let name = match file_name {
        "Makefile" | "makefile" | "GNUmakefile" => "make",
        "CMakeLists.txt" => "cmake",
        "Cargo.lock" => "toml",
        _ => match file_name.rsplit_once('.')?.1 {
            "rs" => "rust",
            "py" => "python",
            "js" | "mjs" | "cjs" | "jsx" => "javascript",
            "ts" | "mts" | "cts" => "typescript",
            "tsx" => "tsx",
            "c" | "h" => "c",
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
            "cs" => "csharp",
            "sh" | "bash" | "zsh" => "bash",
            "md" | "mdx" => "markdown",
            "yml" | "yaml" => "yaml",
            "htm" | "html" => "html",
            "css" | "scss" => "css",
            "kt" | "kts" => "kotlin",
            "rb" => "ruby",
            "ex" | "exs" => "elixir",
            "json" | "jsonc" => "json",
            "go" => "go",
            "java" => "java",
            "lua" => "lua",
            "php" => "php",
            "proto" => "proto",
            "scala" => "scala",
            "sql" => "sql",
            "svelte" => "svelte",
            "swift" => "swift",
            "toml" => "toml",
            "zig" => "zig",
            "cmake" => "cmake",
            "graphql" | "gql" => "graphql",
            _ => return None,
        },
    };
    // Grammars are feature-gated in gpui-component; only claim what is built in.
    (Language::from_str(name) != Language::Plain).then_some(name)
}

/// Fence tags LLMs commonly write that gpui-component does not know, mapped to
/// a grammar that highlights them well enough.
const LANGUAGE_ALIASES: &[(&str, &str)] = &[
    ("shell", "bash"),
    ("zsh", "bash"),
    ("console", "bash"),
    ("shellscript", "bash"),
    ("dockerfile", "bash"),
    ("jsx", "javascript"),
    ("mjs", "javascript"),
    ("golang", "go"),
    ("h", "c"),
    ("hpp", "cpp"),
    ("cc", "cpp"),
    ("cxx", "cpp"),
    ("py3", "python"),
    ("python3", "python"),
    ("htm", "html"),
    ("json5", "json"),
];

/// Make Markdown code blocks tagged with one of [`LANGUAGE_ALIASES`] highlight.
pub fn register_language_aliases() {
    let registry = LanguageRegistry::singleton();
    for (alias, language) in LANGUAGE_ALIASES {
        if let Some(config) = registry.language(language) {
            registry.register(alias, &config);
        }
    }
}

/// One text of a side: the whole side, or one hunk's lines of it.
/// `line_starts[n]` is the byte offset of line `n + 1`.
struct Segment {
    text: String,
    line_starts: Vec<usize>,
}

impl Segment {
    fn new(text: String) -> Self {
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(ix, _)| ix + 1))
            .collect();
        Self { text, line_starts }
    }

    fn line_end(&self, ix: usize) -> usize {
        self.line_starts
            .get(ix + 1)
            .map_or(self.text.len(), |next| next - 1)
    }

    /// One query over the whole text, cut into lines. Styles that are the
    /// default are left out, like empty ranges. `highlighter` must hold this
    /// segment's tree.
    fn style_lines(
        &self,
        highlighter: &SyntaxHighlighter,
        theme: &HighlightTheme,
    ) -> Vec<LineStyles> {
        let mut lines: Vec<LineStyles> = vec![Vec::new(); self.line_starts.len()];
        let mut line_ix = 0;
        for (range, style) in highlighter.styles(&(0..self.text.len()), theme) {
            if style == HighlightStyle::default() || range.is_empty() {
                continue;
            }
            while line_ix + 1 < self.line_starts.len()
                && self.line_starts[line_ix + 1] <= range.start
            {
                line_ix += 1;
            }
            // A style can span lines (block comments, strings): clip it to each.
            let mut ix = line_ix;
            while ix < self.line_starts.len() && self.line_starts[ix] < range.end {
                let start = self.line_starts[ix];
                let clipped = range.start.max(start)..range.end.min(self.line_end(ix));
                if clipped.start < clipped.end {
                    lines[ix].push((clipped.start - start..clipped.end - start, style));
                }
                ix += 1;
            }
        }
        lines
    }

    /// The styles of `line_no` queried on their own, as they were computed
    /// before the cache; the cache has to give the same answer.
    #[cfg(test)]
    fn line_styles_uncached(
        &self,
        highlighter: &SyntaxHighlighter,
        line_no: usize,
        theme: &HighlightTheme,
    ) -> LineStyles {
        let range = self.line_starts[line_no - 1]..self.line_end(line_no - 1);
        highlighter
            .styles(&range, theme)
            .into_iter()
            .filter_map(|(r, style)| {
                let r = r.start.max(range.start) - range.start..r.end.min(range.end) - range.start;
                (r.start < r.end && style != HighlightStyle::default()).then_some((r, style))
            })
            .collect()
    }
}

/// The parser of one side and the styles it gave, under one lock: styling
/// another segment means parsing it first.
struct SideState {
    highlighter: SyntaxHighlighter,
    /// The segment the highlighter's tree is for.
    parsed: Option<usize>,
    /// Filled by [`SyntaxSide::prime`] or on the first query; replaced when
    /// the theme changes.
    cache: Option<StyleCache>,
}

impl SideState {
    fn style_all(&mut self, segments: &[Segment], theme: &HighlightTheme) -> StyleCache {
        let segments = segments
            .iter()
            .enumerate()
            .map(|(ix, segment)| {
                if self.parsed != Some(ix) {
                    self.highlighter
                        .update(None, &Rope::from_str(&segment.text), None);
                    self.parsed = Some(ix);
                }
                segment.style_lines(&self.highlighter, theme)
            })
            .collect();
        StyleCache {
            theme: ThemeKey::of(theme),
            segments,
        }
    }
}

/// One parsed side of a diff.
struct SyntaxSide {
    segments: Vec<Segment>,
    state: Mutex<SideState>,
}

impl SyntaxSide {
    /// Parses the first segment right away; the others are parsed when
    /// their styles are computed.
    fn parse(language: &str, texts: Vec<String>) -> Self {
        let segments: Vec<Segment> = texts.into_iter().map(Segment::new).collect();
        let mut highlighter = SyntaxHighlighter::new(language);
        let parsed = segments.first().map(|first| {
            highlighter.update(None, &Rope::from_str(&first.text), None);
            0
        });
        Self {
            segments,
            state: Mutex::new(SideState {
                highlighter,
                parsed,
                cache: None,
            }),
        }
    }

    /// Compute and keep the styles of every line for `theme`.
    fn prime(&self, theme: &HighlightTheme) {
        let mut state = self.state.lock().unwrap();
        let cache = state.style_all(&self.segments, theme);
        state.cache = Some(cache);
    }

    fn line_styles(
        &self,
        segment: usize,
        line_no: usize,
        line: &str,
        theme: &HighlightTheme,
    ) -> LineStyles {
        let Some(rest) = self.segments.get(segment).and_then(|s| {
            let start = *s.line_starts.get(line_no.checked_sub(1)?)?;
            Some(&s.text[start..])
        }) else {
            return Vec::new();
        };
        // Diff rows and parsed text split lines independently; only style a
        // row that really is this line, so ranges always fit its text.
        if line.is_empty() || !rest.starts_with(line) {
            return Vec::new();
        }
        let key = ThemeKey::of(theme);
        let mut state = self.state.lock().unwrap();
        if state.cache.as_ref().is_none_or(|cache| cache.theme != key) {
            let cache = state.style_all(&self.segments, theme);
            state.cache = Some(cache);
        }
        let Some(cache) = &state.cache else {
            return Vec::new();
        };
        cache.segments[segment][line_no - 1]
            .iter()
            .filter(|(r, _)| r.start < line.len())
            .map(|(r, style)| (r.start..r.end.min(line.len()), *style))
            .collect()
    }
}

/// What a [`DiffSyntax`] parsed, which decides where a row's line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxOrigin {
    /// One text per side, numbered like the file (or the snippet) the rows
    /// show.
    WholeFile,
    /// One text per hunk and side: rows count from their hunk's first line.
    PerHunk,
}

/// Parsed old and new side of one file's diff.
pub struct DiffSyntax {
    old: Option<SyntaxSide>,
    new: Option<SyntaxSide>,
    origin: SyntaxOrigin,
}

impl std::fmt::Debug for DiffSyntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiffSyntax")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl DiffSyntax {
    /// Parse both sides of `path`'s diff, as given: they must split into the
    /// same lines as the rows. `None` when there is no grammar for the file.
    /// CPU-heavy for large files — call on a background thread.
    pub fn parse(path: &str, old: Option<&str>, new: Option<&str>) -> Option<Self> {
        let language = language_for_path(path)?;
        let side =
            |text: Option<&str>| text.map(|t| SyntaxSide::parse(language, vec![t.to_string()]));
        Some(Self {
            old: side(old),
            new: side(new),
            origin: SyntaxOrigin::WholeFile,
        })
    }

    /// Parse each hunk's old and new lines on their own, for diffs that only
    /// have the hunks (a patch). A side no hunk has lines on is left out.
    pub fn parse_hunks(path: &str, hunks: &[DiffHunk]) -> Option<Self> {
        let language = language_for_path(path)?;
        let side = |skip: similar::ChangeTag| {
            let texts: Vec<String> = hunks
                .iter()
                .map(|hunk| {
                    hunk.lines
                        .iter()
                        .filter(|l| l.tag != skip)
                        .map(|l| format!("{}\n", l.text))
                        .collect()
                })
                .collect();
            texts
                .iter()
                .any(|t| !t.is_empty())
                .then(|| SyntaxSide::parse(language, texts))
        };
        Some(Self {
            old: side(ChangeTag::Insert),
            new: side(ChangeTag::Delete),
            origin: SyntaxOrigin::PerHunk,
        })
    }

    pub fn origin(&self) -> SyntaxOrigin {
        self.origin
    }

    /// Compute the styles of every line of both sides for `theme` now, so
    /// the first frames do not have to. Call on the background thread that
    /// parsed.
    pub fn prime(&self, theme: &HighlightTheme) {
        for side in [&self.old, &self.new].into_iter().flatten() {
            side.prime(theme);
        }
    }

    /// Syntax styles for one diff row, as ranges into `line`. `segment` is
    /// the hunk for [`SyntaxOrigin::PerHunk`], else 0; `line_no` is 1-based
    /// in the row's own side (the old text for deletions, the new text
    /// otherwise) of that segment. Cheap after the first call per theme.
    pub fn line_styles(
        &self,
        tag: ChangeTag,
        segment: usize,
        line_no: usize,
        line: &str,
        theme: &HighlightTheme,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let side = match tag {
            ChangeTag::Delete => &self.old,
            ChangeTag::Equal | ChangeTag::Insert => &self.new,
        };
        side.as_ref()
            .map(|side| side.line_styles(segment, line_no, line, theme))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_is_detected_from_extension_and_file_name() {
        assert_eq!(language_for_path("crates/ui/src/lib.rs"), Some("rust"));
        assert_eq!(language_for_path("Cargo.lock"), Some("toml"));
        assert_eq!(language_for_path("build/Makefile"), Some("make"));
        assert_eq!(language_for_path("notes.txt"), None);
        assert_eq!(language_for_path("LICENSE"), None);
    }

    #[test]
    fn common_fence_tags_resolve_to_a_grammar() {
        register_language_aliases();
        let registry = LanguageRegistry::singleton();
        for (tag, language) in [
            ("shell", "bash"),
            ("zsh", "bash"),
            ("console", "bash"),
            ("jsx", "javascript"),
            ("golang", "go"),
            ("h", "c"),
            ("hpp", "cpp"),
            ("dockerfile", "bash"),
            // Built into gpui-component already.
            ("rs", "rust"),
            ("py", "python"),
            ("yml", "yaml"),
        ] {
            let config = registry.language(tag);
            assert_eq!(
                config.as_ref().map(|c| c.name.as_ref()),
                Some(language),
                "fence tag {tag}"
            );
            assert!(config.unwrap().has_grammar());
        }
        assert!(registry.language("no-such-language").is_none());
    }

    #[test]
    fn no_grammar_means_no_syntax() {
        assert!(DiffSyntax::parse("notes.txt", Some("a"), Some("b")).is_none());
    }

    #[test]
    fn line_styles_are_relative_to_the_row_and_pick_the_right_side() {
        let old = "fn old_name() {}\n";
        let new = "// intro\nfn new_name() {}\n";
        let syntax = DiffSyntax::parse("a.rs", Some(old), Some(new)).unwrap();
        let theme = HighlightTheme::default_dark();

        let keyword_at_start =
            |styles: &[(Range<usize>, HighlightStyle)]| styles.iter().any(|(r, _)| *r == (0..2));

        // New line 2 is the `fn`; on the old side it is line 1.
        let inserted = syntax.line_styles(ChangeTag::Insert, 0, 2, "fn new_name() {}", &theme);
        assert!(keyword_at_start(&inserted), "{inserted:?}");
        let deleted = syntax.line_styles(ChangeTag::Delete, 0, 1, "fn old_name() {}", &theme);
        assert!(keyword_at_start(&deleted), "{deleted:?}");

        for (range, _) in inserted.iter().chain(&deleted) {
            assert!(range.end <= "fn new_name() {}".len());
        }
    }

    #[test]
    fn rows_that_do_not_match_the_parsed_line_get_no_styles() {
        let syntax = DiffSyntax::parse("a.rs", None, Some("fn main() {}\n")).unwrap();
        let theme = HighlightTheme::default_dark();
        assert!(
            syntax
                .line_styles(ChangeTag::Insert, 0, 1, "let x = 1;", &theme)
                .is_empty()
        );
        assert!(
            syntax
                .line_styles(ChangeTag::Insert, 0, 9, "fn main() {}", &theme)
                .is_empty()
        );
        assert!(
            syntax
                .line_styles(ChangeTag::Insert, 0, 0, "fn main() {}", &theme)
                .is_empty()
        );
        // No old side was parsed.
        assert!(
            syntax
                .line_styles(ChangeTag::Delete, 0, 1, "fn main() {}", &theme)
                .is_empty()
        );
    }

    #[test]
    fn cached_line_styles_match_per_line_queries() {
        let text =
            "/* a comment\n   over two lines */\nfn main() {\n    let s = \"multi\nline\";\n}\n";
        let side = SyntaxSide::parse("rust", vec![text.to_string()]);
        let theme = HighlightTheme::default_dark();
        let state = side.state.lock().unwrap();
        let segment = &side.segments[0];
        let cached = segment.style_lines(&state.highlighter, &theme);
        assert_eq!(cached.len(), segment.line_starts.len());
        for line_no in 1..=segment.line_starts.len() {
            assert_eq!(
                cached[line_no - 1],
                segment.line_styles_uncached(&state.highlighter, line_no, &theme),
                "line {line_no}"
            );
        }
        // The comment's style reaches the second line, relative to that line.
        assert_eq!(cached[1].first().map(|(r, _)| r.start), Some(0));
    }

    #[test]
    fn line_styles_are_computed_once_per_theme() {
        let syntax = DiffSyntax::parse("a.rs", Some("fn a() {}\n"), Some("fn b() {}\n")).unwrap();
        let dark = HighlightTheme::default_dark();
        let light = HighlightTheme::default_light();
        assert!(
            syntax
                .new
                .as_ref()
                .unwrap()
                .state
                .lock()
                .unwrap()
                .cache
                .is_none()
        );

        syntax.prime(&dark);
        let cached_theme = |side: &SyntaxSide| {
            side.state
                .lock()
                .unwrap()
                .cache
                .as_ref()
                .map(|c| c.theme.clone())
        };
        assert_eq!(
            cached_theme(syntax.new.as_ref().unwrap()),
            Some(ThemeKey::of(&dark))
        );
        assert_eq!(
            cached_theme(syntax.old.as_ref().unwrap()),
            Some(ThemeKey::of(&dark))
        );

        // Same theme: served from the cache; another theme replaces it.
        assert!(
            !syntax
                .line_styles(ChangeTag::Insert, 0, 1, "fn b() {}", &dark)
                .is_empty()
        );
        assert_eq!(
            cached_theme(syntax.new.as_ref().unwrap()),
            Some(ThemeKey::of(&dark))
        );
        assert!(
            !syntax
                .line_styles(ChangeTag::Insert, 0, 1, "fn b() {}", &light)
                .is_empty()
        );
        assert_eq!(
            cached_theme(syntax.new.as_ref().unwrap()),
            Some(ThemeKey::of(&light))
        );
    }

    fn patch_hunks(patch: &str) -> Vec<DiffHunk> {
        crate::diff::model::hunks_from_patch(&git::parse_hunks(patch))
    }

    #[test]
    fn hunks_are_parsed_on_their_own() {
        // The first hunk ends inside a block comment; the second hunk's code
        // must not be colored as comment.
        let hunks = patch_hunks(
            "@@ -1,2 +1,2 @@\n-fn a() {}\n+fn b() {}\n /* open\n\
             @@ -40 +40,2 @@\n fn c() {}\n+fn d() {}\n",
        );
        let syntax = DiffSyntax::parse_hunks("a.rs", &hunks).unwrap();
        assert_eq!(syntax.origin(), SyntaxOrigin::PerHunk);
        let theme = HighlightTheme::default_dark();
        syntax.prime(&theme);
        let keyword_at_start =
            |styles: Vec<(Range<usize>, HighlightStyle)>| styles.iter().any(|(r, _)| *r == (0..2));
        let comment = syntax.line_styles(ChangeTag::Equal, 0, 2, "/* open", &theme);
        let fn_c = syntax.line_styles(ChangeTag::Equal, 1, 1, "fn c() {}", &theme);
        assert_ne!(comment.first().map(|s| s.1), fn_c.first().map(|s| s.1));
        assert!(keyword_at_start(fn_c));
        assert!(keyword_at_start(syntax.line_styles(
            ChangeTag::Insert,
            1,
            2,
            "fn d() {}",
            &theme
        )));
        assert!(keyword_at_start(syntax.line_styles(
            ChangeTag::Delete,
            0,
            1,
            "fn a() {}",
            &theme
        )));
        // Old line 2 of hunk 1 does not exist (it has one old line).
        assert!(
            syntax
                .line_styles(ChangeTag::Delete, 1, 2, "fn d() {}", &theme)
                .is_empty()
        );

        // Another theme re-parses each hunk for its styles.
        let light = HighlightTheme::default_light();
        assert!(keyword_at_start(syntax.line_styles(
            ChangeTag::Insert,
            0,
            1,
            "fn b() {}",
            &light
        )));
        assert!(keyword_at_start(syntax.line_styles(
            ChangeTag::Insert,
            1,
            2,
            "fn d() {}",
            &light
        )));
    }

    #[test]
    fn a_side_without_lines_is_not_parsed() {
        let hunks = patch_hunks("@@ -0,0 +1,2 @@\n+fn a() {}\n+fn b() {}\n");
        let syntax = DiffSyntax::parse_hunks("a.rs", &hunks).unwrap();
        assert!(syntax.old.is_none());
        assert!(syntax.new.is_some());
        assert!(DiffSyntax::parse_hunks("notes.txt", &hunks).is_none());
    }

    #[test]
    fn diff_syntax_can_cross_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DiffSyntax>();
    }
}
