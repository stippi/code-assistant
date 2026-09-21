//! Syntax highlighting for diff rows. Both sides of a diff are parsed once
//! (tree-sitter, via gpui-component's [`SyntaxHighlighter`]) on a background
//! thread; rendering then asks for the styles of just the lines it builds,
//! against the theme current at that moment.

use super::diff_card::normalize_for_diff;
use gpui::HighlightStyle;
use gpui_component::Rope;
use gpui_component::highlighter::{HighlightTheme, Language, LanguageRegistry, SyntaxHighlighter};
use similar::ChangeTag;
use std::ops::Range;

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

/// One parsed side of a diff. `line_starts[n]` is the byte offset of line
/// `n + 1` in the parsed text.
struct SyntaxSide {
    highlighter: SyntaxHighlighter,
    text: String,
    line_starts: Vec<usize>,
}

impl SyntaxSide {
    fn parse(language: &str, text: &str) -> Self {
        // The same normalization the line diff applies, so line numbers agree.
        let text = normalize_for_diff(text);
        let mut highlighter = SyntaxHighlighter::new(language);
        highlighter.update(None, &Rope::from_str(&text), None);
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(ix, _)| ix + 1))
            .collect();
        Self {
            highlighter,
            text,
            line_starts,
        }
    }

    fn line_styles(
        &self,
        line_no: usize,
        line: &str,
        theme: &HighlightTheme,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let Some(&start) = line_no
            .checked_sub(1)
            .and_then(|ix| self.line_starts.get(ix))
        else {
            return Vec::new();
        };
        // Diff rows and parsed text split lines independently; only style a
        // row that really is this line, so ranges always fit its text.
        if line.is_empty() || !self.text[start..].starts_with(line) {
            return Vec::new();
        }
        let range = start..start + line.len();
        self.highlighter
            .styles(&range, theme)
            .into_iter()
            .filter_map(|(r, style)| {
                let r = r.start.max(range.start) - start..r.end.min(range.end) - start;
                (r.start < r.end && style != HighlightStyle::default()).then_some((r, style))
            })
            .collect()
    }
}

/// Parsed old and new side of one file's diff.
pub struct DiffSyntax {
    old: Option<SyntaxSide>,
    new: Option<SyntaxSide>,
}

impl std::fmt::Debug for DiffSyntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiffSyntax").finish_non_exhaustive()
    }
}

impl DiffSyntax {
    /// Parse both sides of `path`'s diff. `None` when there is no grammar for
    /// the file. CPU-heavy for large files — call on a background thread.
    pub fn parse(path: &str, old: Option<&str>, new: Option<&str>) -> Option<Self> {
        let language = language_for_path(path)?;
        Some(Self {
            old: old.map(|text| SyntaxSide::parse(language, text)),
            new: new.map(|text| SyntaxSide::parse(language, text)),
        })
    }

    /// Syntax styles for one diff row, as ranges into `line`. `line_no` is
    /// 1-based in the row's own side: the old file for deletions, the new
    /// file otherwise.
    pub fn line_styles(
        &self,
        tag: ChangeTag,
        line_no: usize,
        line: &str,
        theme: &HighlightTheme,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let side = match tag {
            ChangeTag::Delete => &self.old,
            ChangeTag::Equal | ChangeTag::Insert => &self.new,
        };
        side.as_ref()
            .map(|side| side.line_styles(line_no, line, theme))
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
        let inserted = syntax.line_styles(ChangeTag::Insert, 2, "fn new_name() {}", &theme);
        assert!(keyword_at_start(&inserted), "{inserted:?}");
        let deleted = syntax.line_styles(ChangeTag::Delete, 1, "fn old_name() {}", &theme);
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
                .line_styles(ChangeTag::Insert, 1, "let x = 1;", &theme)
                .is_empty()
        );
        assert!(
            syntax
                .line_styles(ChangeTag::Insert, 9, "fn main() {}", &theme)
                .is_empty()
        );
        assert!(
            syntax
                .line_styles(ChangeTag::Insert, 0, "fn main() {}", &theme)
                .is_empty()
        );
        // No old side was parsed.
        assert!(
            syntax
                .line_styles(ChangeTag::Delete, 1, "fn main() {}", &theme)
                .is_empty()
        );
    }

    #[test]
    fn diff_syntax_can_cross_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DiffSyntax>();
    }
}
