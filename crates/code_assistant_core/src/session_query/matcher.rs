//! Declarative string matchers and their compiled, validated form.
//!
//! A [`StringMatch`] is the intuitive, serializable shape a caller (or an LLM
//! tool) writes — e.g. `{"contains": "docs/"}` or `{"glob": "**/*.md"}`.
//! [`StringMatch::compile`] validates it once (surfacing a bad regex/glob as
//! an error instead of silently never matching) into a [`CompiledMatch`] that
//! can be applied to many candidate strings.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// How to match a string value.
///
/// Serialized in externally-tagged form so the JSON is self-describing:
/// `{"equals": "..."}`, `{"contains": "..."}`, `{"glob": "..."}`,
/// `{"regex": "..."}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StringMatch {
    /// The value equals this string exactly.
    Equals(String),
    /// The value contains this substring.
    Contains(String),
    /// The value matches this glob pattern (`*`, `?`, `[..]`; `*` spans path
    /// separators).
    Glob(String),
    /// The value matches this regular expression (unanchored).
    Regex(String),
}

/// A validated, ready-to-apply matcher.
#[derive(Debug, Clone)]
pub enum CompiledMatch {
    Equals(String),
    Contains(String),
    Glob(glob::Pattern),
    Regex(regex::Regex),
}

impl StringMatch {
    /// Validate and compile the matcher. Returns an error with context for an
    /// invalid glob or regular expression.
    pub fn compile(&self) -> Result<CompiledMatch> {
        Ok(match self {
            StringMatch::Equals(s) => CompiledMatch::Equals(s.clone()),
            StringMatch::Contains(s) => CompiledMatch::Contains(s.clone()),
            StringMatch::Glob(pattern) => CompiledMatch::Glob(
                glob::Pattern::new(pattern)
                    .with_context(|| format!("invalid glob pattern: {pattern:?}"))?,
            ),
            StringMatch::Regex(pattern) => CompiledMatch::Regex(
                regex::Regex::new(pattern)
                    .with_context(|| format!("invalid regular expression: {pattern:?}"))?,
            ),
        })
    }
}

impl CompiledMatch {
    /// Whether `value` matches.
    pub fn is_match(&self, value: &str) -> bool {
        match self {
            CompiledMatch::Equals(s) => value == s,
            CompiledMatch::Contains(s) => value.contains(s.as_str()),
            CompiledMatch::Glob(pattern) => pattern.matches(value),
            CompiledMatch::Regex(re) => re.is_match(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equals_matches_exactly() {
        let m = StringMatch::Equals("docs/readme.md".into())
            .compile()
            .unwrap();
        assert!(m.is_match("docs/readme.md"));
        assert!(!m.is_match("docs/readme.md.bak"));
        assert!(!m.is_match("readme.md"));
    }

    #[test]
    fn contains_matches_substring() {
        let m = StringMatch::Contains("docs/".into()).compile().unwrap();
        assert!(m.is_match("selfhosting-poc/docs/plan.md"));
        assert!(!m.is_match("src/main.rs"));
    }

    #[test]
    fn glob_matches_across_separators() {
        let m = StringMatch::Glob("**/docs/*.md".into()).compile().unwrap();
        assert!(m.is_match("a/b/docs/plan.md"));

        let star = StringMatch::Glob("docs/*.md".into()).compile().unwrap();
        // `*` spans path separators by default, matching contains-like usage.
        assert!(star.is_match("docs/sub/plan.md"));
        assert!(star.is_match("docs/plan.md"));
        assert!(!star.is_match("docs/plan.txt"));
    }

    #[test]
    fn regex_matches_unanchored() {
        let m = StringMatch::Regex(r"docs/.*\.md$".into())
            .compile()
            .unwrap();
        assert!(m.is_match("selfhosting-poc/docs/plan.md"));
        assert!(!m.is_match("docs/plan.rs"));
    }

    #[test]
    fn invalid_regex_is_reported() {
        let err = StringMatch::Regex("(".into()).compile().unwrap_err();
        assert!(format!("{err}").contains("regular expression"));
    }

    #[test]
    fn invalid_glob_is_reported() {
        let err = StringMatch::Glob("a[".into()).compile().unwrap_err();
        assert!(format!("{err}").contains("glob"));
    }

    #[test]
    fn json_shape_is_self_describing() {
        let m = StringMatch::Contains("docs/".into());
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json, serde_json::json!({ "contains": "docs/" }));

        let parsed: StringMatch =
            serde_json::from_value(serde_json::json!({ "glob": "**/*.md" })).unwrap();
        assert_eq!(parsed, StringMatch::Glob("**/*.md".into()));
    }
}
