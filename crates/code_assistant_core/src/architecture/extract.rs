//! Per-file facts for the architecture overview, read without a model: the
//! file's doc comment, its public items and what it imports.
//!
//! The extractors are line-oriented heuristics, not parsers. They only have
//! to be good enough to tell a model (and a reader) what a part exposes and
//! which parts it uses; a missed item costs detail, never correctness.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// Signatures kept per file.
const MAX_ITEMS: usize = 40;
/// A signature is cut beyond this many characters.
const MAX_SIGNATURE: usize = 140;
/// A doc comment is cut beyond this many characters.
const MAX_DOC: usize = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    /// Source in a language without an extractor: counted, not read.
    Other,
}

impl Language {
    pub fn from_path(path: &str) -> Option<Self> {
        let ext = path.rsplit_once('.')?.1;
        Some(match ext {
            "rs" => Self::Rust,
            "ts" | "tsx" | "mts" | "cts" => Self::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "py" => Self::Python,
            "go" | "java" | "kt" | "kts" | "swift" | "c" | "h" | "cc" | "cpp" | "hpp" | "cs"
            | "rb" | "php" | "scala" | "m" | "mm" | "vue" | "svelte" | "dart" | "ex" | "exs"
            | "zig" => Self::Other,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::TypeScript => "TypeScript",
            Self::JavaScript => "JavaScript",
            Self::Python => "Python",
            Self::Other => "other",
        }
    }
}

/// A reference to other code, before it is resolved against the project.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Import {
    /// A Rust path: `crate`, `super`, `self` or a crate name, then segments.
    RustPath(Vec<String>),
    /// A relative module specifier (`./x`, `../y/z`) of a JS/TS file.
    Relative(String),
    /// A bare JS/TS module specifier (`react`, `@scope/pkg/sub`).
    Package(String),
    /// A Python import: `level` leading dots, then the dotted module.
    Python { level: usize, module: Vec<String> },
}

/// What a source file tells about itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileFacts {
    pub lines: usize,
    /// The file's leading doc comment (module doc, docstring, header).
    pub doc: Option<String>,
    /// Public items, as one-line signatures.
    pub items: Vec<String>,
    pub imports: Vec<Import>,
}

pub fn extract(language: Language, text: &str) -> FileFacts {
    let mut facts = match language {
        Language::Rust => rust(text),
        Language::TypeScript | Language::JavaScript => script(text),
        Language::Python => python(text),
        Language::Other => FileFacts::default(),
    };
    facts.lines = text.lines().count();
    facts.items.truncate(MAX_ITEMS);
    facts.imports.sort_by_key(|i| format!("{i:?}"));
    facts.imports.dedup();
    facts
}

fn clip(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        text
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}

fn clean_doc(lines: Vec<String>) -> Option<String> {
    let text = lines.join("\n").trim().to_owned();
    (!text.is_empty()).then(|| {
        if text.chars().count() > MAX_DOC {
            format!("{}…", text.chars().take(MAX_DOC).collect::<String>())
        } else {
            text
        }
    })
}

/// Join a declaration's lines until it opens a body or ends, so a signature
/// split over lines reads as one.
fn signature(lines: &[&str], start: usize, ends: &[char]) -> String {
    let mut sig = String::new();
    for line in lines.iter().skip(start).take(6) {
        let line = line.trim();
        if let Some(ix) = line.find(ends) {
            sig.push_str(&line[..ix]);
            break;
        }
        sig.push_str(line);
        sig.push(' ');
    }
    clip(
        sig.trim().trim_end_matches(['=', ',']).trim(),
        MAX_SIGNATURE,
    )
}

// ---------------------------------------------------------------------------
// Rust
// ---------------------------------------------------------------------------

static RUST_ITEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^pub\s+(?:(?:async|unsafe|const|extern\s+.[A-Za-z]+.)\s+)*(fn|struct|enum|trait|type|const|static|union|use|macro_rules!)\b",
    )
    .unwrap()
});
static RUST_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([a-z_][a-z0-9_]*)((?:::[A-Za-z_][A-Za-z0-9_]*)+)").unwrap());

fn rust(text: &str) -> FileFacts {
    let lines: Vec<&str> = text.lines().collect();
    let mut facts = FileFacts::default();

    let mut doc = Vec::new();
    for line in &lines {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("//!") {
            doc.push(rest.strip_prefix(' ').unwrap_or(rest).to_owned());
        } else if trimmed.is_empty() && doc.is_empty() || trimmed.starts_with("#![") {
            continue;
        } else {
            break;
        }
    }
    facts.doc = clean_doc(doc);

    let mut ix = 0;
    while ix < lines.len() {
        let line = lines[ix];
        let trimmed = line.trim();
        // Unit tests sit at the end of a file and expose nothing.
        if trimmed == "#[cfg(test)]"
            && lines
                .get(ix + 1)
                .is_some_and(|next| next.trim_start().starts_with("mod "))
        {
            break;
        }
        if trimmed.starts_with("//") {
            ix += 1;
            continue;
        }
        if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") {
            let mut statement = String::new();
            let mut end = ix;
            while end < lines.len() {
                statement.push_str(lines[end].trim());
                statement.push(' ');
                if lines[end].contains(';') {
                    break;
                }
                end += 1;
            }
            let body = statement
                .trim()
                .trim_start_matches("pub ")
                .trim_start_matches("use ")
                .trim_end()
                .trim_end_matches(';');
            for path in expand_use_tree(body) {
                facts.imports.push(Import::RustPath(path));
            }
        }
        // Only items at the top level: indented ones belong to impls and
        // inline modules.
        if !line.starts_with(char::is_whitespace) && RUST_ITEM.is_match(trimmed) {
            // A re-export's braces are part of it.
            let ends: &[char] = if trimmed.starts_with("pub use ") {
                &[';']
            } else {
                &['{', ';']
            };
            facts.items.push(signature(&lines, ix, ends));
        }
        for cap in RUST_PATH.captures_iter(trimmed) {
            let first = &cap[1];
            if matches!(first, "std" | "core" | "alloc") {
                continue;
            }
            let mut path = vec![first.to_owned()];
            path.extend(
                cap[2]
                    .split("::")
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            );
            facts.imports.push(Import::RustPath(path));
        }
        ix += 1;
    }
    facts
}

/// `a::{b, c::{d, e as f}, g::*}` → `a::b`, `a::c::d`, `a::c::e`, `a::g`.
fn expand_use_tree(tree: &str) -> Vec<Vec<String>> {
    fn split_top(list: &str) -> Vec<&str> {
        let mut parts = Vec::new();
        let (mut depth, mut start) = (0, 0);
        for (i, c) in list.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(&list[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        parts.push(&list[start..]);
        parts
    }
    fn walk(prefix: &[String], tree: &str, out: &mut Vec<Vec<String>>) {
        let tree = tree.trim();
        if tree.is_empty() {
            return;
        }
        if let Some(open) = tree.find('{') {
            let head = tree[..open].trim().trim_end_matches("::");
            let inner = tree[open + 1..].trim_end().trim_end_matches('}');
            let mut base = prefix.to_vec();
            base.extend(
                head.split("::")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            );
            for part in split_top(inner) {
                walk(&base, part, out);
            }
            return;
        }
        let tree = tree.split(" as ").next().unwrap_or(tree);
        let mut path = prefix.to_vec();
        path.extend(
            tree.split("::")
                .map(str::trim)
                .filter(|s| !s.is_empty() && *s != "*" && *s != "self")
                .map(str::to_owned),
        );
        if !path.is_empty() {
            out.push(path);
        }
    }
    let mut out = Vec::new();
    walk(&[], tree.trim_start_matches("::"), &mut out);
    out
}

// ---------------------------------------------------------------------------
// TypeScript / JavaScript
// ---------------------------------------------------------------------------

static SCRIPT_ITEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^export\s+(?:default\s+)?(?:declare\s+)?(?:async\s+)?(?:abstract\s+)?(function\*?|class|interface|type|const|let|var|enum|namespace)\b",
    )
    .unwrap()
});
static SCRIPT_REEXPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^export\s+(?:\*|\{[^}]*\}|default\b)").unwrap());
static SCRIPT_FROM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:^|[\s;])(?:import|export)\s[^'";]*?\bfrom\s*['"]([^'"]+)['"]"#).unwrap()
});
static SCRIPT_BARE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:^|[\s;])import\s*['"]([^'"]+)['"]|\b(?:require|import)\(\s*['"]([^'"]+)['"]\s*\)"#,
    )
    .unwrap()
});

fn script(text: &str) -> FileFacts {
    let lines: Vec<&str> = text.lines().collect();
    let mut facts = FileFacts {
        doc: script_header(&lines),
        ..FileFacts::default()
    };
    for (ix, line) in lines.iter().enumerate() {
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        if SCRIPT_ITEM.is_match(line) {
            facts.items.push(signature(&lines, ix, &['{', '=', ';']));
        } else if SCRIPT_REEXPORT.is_match(line) {
            facts
                .items
                .push(clip(line.trim_end_matches(';'), MAX_SIGNATURE));
        }
    }
    let specs = SCRIPT_FROM
        .captures_iter(text)
        .filter_map(|c| c.get(1))
        .chain(
            SCRIPT_BARE
                .captures_iter(text)
                .filter_map(|c| c.get(1).or_else(|| c.get(2))),
        );
    for spec in specs {
        let spec = spec.as_str();
        facts.imports.push(if spec.starts_with('.') {
            Import::Relative(spec.to_owned())
        } else {
            Import::Package(spec.to_owned())
        });
    }
    facts
}

/// The comment a script file opens with.
fn script_header(lines: &[&str]) -> Option<String> {
    let mut doc = Vec::new();
    let mut in_block = false;
    for line in lines {
        let trimmed = line.trim();
        if in_block {
            let (content, done) = match trimmed.find("*/") {
                Some(ix) => (&trimmed[..ix], true),
                None => (trimmed, false),
            };
            doc.push(content.trim_start_matches('*').trim().to_owned());
            if done {
                break;
            }
        } else if let Some(rest) = trimmed.strip_prefix("/*") {
            let rest = rest.trim_start_matches('*');
            if let Some(ix) = rest.find("*/") {
                doc.push(rest[..ix].trim().to_owned());
                break;
            }
            doc.push(rest.trim().to_owned());
            in_block = true;
        } else if let Some(rest) = trimmed.strip_prefix("//") {
            doc.push(rest.trim().to_owned());
        } else if trimmed.is_empty() && doc.is_empty()
            || trimmed.starts_with("#!")
            || trimmed.starts_with("'use ")
            || trimmed.starts_with("\"use ")
        {
            continue;
        } else {
            break;
        }
    }
    clean_doc(doc)
}

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

static PY_ITEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:async\s+)?(?:def|class)\s+([A-Za-z][A-Za-z0-9_]*)").unwrap());
static PY_FROM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*from\s+(\.*)([\w\.]*)\s+import\s+(.+)$").unwrap());
static PY_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*import\s+([\w\.]+(?:\s*,\s*[\w\.]+)*)").unwrap());

fn python(text: &str) -> FileFacts {
    let lines: Vec<&str> = text.lines().collect();
    let mut facts = FileFacts {
        doc: python_docstring(&lines),
        ..FileFacts::default()
    };
    for (ix, line) in lines.iter().enumerate() {
        if PY_ITEM.is_match(line) {
            facts.items.push(python_signature(&lines, ix));
        } else if line.starts_with("__all__") {
            facts.items.push(clip(line, MAX_SIGNATURE));
        }
        if let Some(cap) = PY_FROM.captures(line) {
            let level = cap[1].len();
            let module: Vec<String> = cap[2]
                .split('.')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            // `from pkg import sub` may name a submodule: offer both.
            let names = cap[3].trim_matches(|c| c == '(' || c == ')' || c == ' ');
            for name in names.split(',') {
                let name = name.split_whitespace().next().unwrap_or("");
                if name.is_empty() || name == "*" || name == "(" {
                    continue;
                }
                let mut sub = module.clone();
                sub.push(name.to_owned());
                facts.imports.push(Import::Python { level, module: sub });
            }
            facts.imports.push(Import::Python { level, module });
        } else if let Some(cap) = PY_IMPORT.captures(line) {
            for module in cap[1].split(',') {
                facts.imports.push(Import::Python {
                    level: 0,
                    module: module.trim().split('.').map(str::to_owned).collect(),
                });
            }
        }
    }
    facts
}

/// A `def`/`class` header up to its closing `:`; annotations contain `:`
/// too, so the header ends where the parentheses balance.
fn python_signature(lines: &[&str], start: usize) -> String {
    let mut sig = String::new();
    let mut depth = 0i32;
    for line in lines.iter().skip(start).take(8) {
        let line = line.trim();
        for c in line.chars() {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                _ => {}
            }
        }
        sig.push_str(line);
        sig.push(' ');
        if depth <= 0 && line.ends_with(':') {
            break;
        }
    }
    clip(sig.trim().trim_end_matches(':'), MAX_SIGNATURE)
}

fn python_docstring(lines: &[&str]) -> Option<String> {
    let mut iter = lines
        .iter()
        .map(|l| l.trim())
        .skip_while(|l| l.is_empty() || l.starts_with('#'));
    let first = iter.next()?;
    let quote = ["\"\"\"", "'''"]
        .into_iter()
        .find(|q| first.starts_with(q))?;
    let rest = &first[3..];
    if let Some(end) = rest.find(quote) {
        return clean_doc(vec![rest[..end].to_owned()]);
    }
    let mut doc = vec![rest.to_owned()];
    for line in iter {
        if let Some(end) = line.find(quote) {
            doc.push(line[..end].to_owned());
            break;
        }
        doc.push(line.to_owned());
    }
    clean_doc(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_paths(facts: &FileFacts) -> Vec<String> {
        facts
            .imports
            .iter()
            .filter_map(|i| match i {
                Import::RustPath(p) => Some(p.join("::")),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn rust_doc_items_and_imports() {
        let text = r#"//! Talks to the model.
//! Second line.

use crate::session::{SessionService, event_stream::{EventStream as Es}};
use llm::LLMProvider;

pub struct Client {
    inner: u32,
}

impl Client {
    pub fn new() -> Self { todo!() }
}

pub async fn send(
    client: &Client,
) -> Result<()> {
    crate::tools::run();
}

pub(crate) fn hidden() {}

#[cfg(test)]
mod tests {
    pub fn not_exported() {}
}
"#;
        let facts = extract(Language::Rust, text);
        assert_eq!(
            facts.doc.as_deref(),
            Some("Talks to the model.\nSecond line.")
        );
        assert_eq!(
            facts.items,
            vec![
                "pub struct Client".to_owned(),
                "pub async fn send( client: &Client, ) -> Result<()>".to_owned(),
            ]
        );
        let paths = rust_paths(&facts);
        assert!(paths.contains(&"crate::session::SessionService".to_owned()));
        assert!(paths.contains(&"crate::session::event_stream::EventStream".to_owned()));
        assert!(paths.contains(&"llm::LLMProvider".to_owned()));
        assert!(paths.contains(&"crate::tools::run".to_owned()));
        assert!(!paths.iter().any(|p| p.contains("not_exported")));
    }

    #[test]
    fn use_trees_expand() {
        let paths = expand_use_tree("a::{b, c::{d, e as f}, g::*, self}");
        let joined: Vec<String> = paths.iter().map(|p| p.join("::")).collect();
        assert_eq!(joined, vec!["a::b", "a::c::d", "a::c::e", "a::g", "a"]);
    }

    #[test]
    fn typescript_exports_and_imports() {
        let text = r#"/**
 * The store of the app.
 */
import { a,
  b } from './util';
import React from "react";
import './styles.css';
const lazy = () => import('../pages/home');

export interface Store {
  x: number;
}
export const createStore = (init: number): Store => ({ x: init });
export default class App {}
export * from './types';
function internal() {}
"#;
        let facts = extract(Language::TypeScript, text);
        assert_eq!(facts.doc.as_deref(), Some("The store of the app."));
        assert_eq!(
            facts.items,
            vec![
                "export interface Store",
                "export const createStore",
                "export default class App",
                "export * from './types'",
            ]
        );
        for expected in [
            Import::Relative("./util".into()),
            Import::Package("react".into()),
            Import::Relative("./styles.css".into()),
            Import::Relative("../pages/home".into()),
            Import::Relative("./types".into()),
        ] {
            assert!(facts.imports.contains(&expected), "{expected:?}");
        }
    }

    #[test]
    fn python_docstring_items_and_imports() {
        let text = r#""""Billing: invoices and payments."""
import os, app.models
from . import utils
from ..core.db import Session, engine

class Invoice(Base):
    def total(self):
        pass

def create_invoice(customer: str) -> Invoice:
    pass

def _private():
    pass
"#;
        let facts = extract(Language::Python, text);
        assert_eq!(
            facts.doc.as_deref(),
            Some("Billing: invoices and payments.")
        );
        assert_eq!(
            facts.items,
            vec![
                "class Invoice(Base)",
                "def create_invoice(customer: str) -> Invoice",
            ]
        );
        let imports = &facts.imports;
        let py = |level, m: &str| Import::Python {
            level,
            module: m
                .split('.')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        };
        assert!(imports.contains(&py(0, "app.models")));
        assert!(imports.contains(&py(1, "utils")));
        assert!(imports.contains(&py(2, "core.db.Session")));
        assert!(imports.contains(&py(2, "core.db")));
    }
}
