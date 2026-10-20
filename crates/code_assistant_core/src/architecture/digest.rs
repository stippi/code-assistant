//! What the model sees of a component, and what it answers.
//!
//! A digest is a compact text built only from deterministic facts: the
//! component's description and interface, and per part its doc, exposed
//! items and dependencies. No function bodies — so editing code inside a
//! function leaves the digest, and with it the cached summary, untouched.
//! The digest's hash is the cache key ([`digest_key`]).

use super::scan::{ArchitectureModel, ComponentKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

/// Bump when the digest format or the prompt changes, to retire cached
/// summaries made from the old ones.
const PROMPT_VERSION: &str = "architecture-v1";
/// Characters of a digest, roughly 4k tokens.
const DIGEST_BUDGET: usize = 16_000;
/// Parts described in a digest; further ones are only named.
const MAX_PARTS: usize = 80;

pub const SYSTEM_PROMPT: &str = r#"You are a software architect documenting a codebase for developers who will not read its code. You get a digest of one component of the project: its description and public interface, then its parts with their docs, exposed items and which sibling parts they use.

Answer with a single JSON object and nothing else:
{
  "summary": "one sentence (at most 20 words): what this component is for",
  "responsibility": "2-4 sentences: what it is responsible for and how its parts work together",
  "constraints": ["up to 5 architectural rules or invariants the structure shows: dependency direction and layering, what it must not depend on, concurrency or state ownership, extension points"],
  "interfaces": [{"name": "an important exposed item", "description": "what the rest of the system uses it for"}],
  "parts": {"<part name exactly as given>": "at most 12 words: the part's responsibility"}
}

Rules: describe every listed part in "parts". Use at most 6 interfaces. Only state constraints the digest supports. Be concrete and brief; no filler, no marketing language."#;

/// A component explained by the model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComponentSummary {
    pub summary: String,
    pub responsibility: String,
    pub constraints: Vec<String>,
    pub interfaces: Vec<InterfaceNote>,
    /// Part name → its one-line responsibility.
    pub parts: BTreeMap<String, String>,
    /// The model that wrote it.
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterfaceNote {
    pub name: String,
    pub description: String,
}

/// Interfaces may come back as plain strings.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawInterface {
    Note(InterfaceNote),
    Text(String),
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSummary {
    summary: String,
    responsibility: String,
    constraints: Vec<String>,
    interfaces: Vec<RawInterface>,
    parts: BTreeMap<String, String>,
}

/// Parse the model's answer; tolerant of code fences and prose around the
/// JSON object.
pub fn parse_summary(text: &str) -> anyhow::Result<ComponentSummary> {
    let start = text
        .find('{')
        .ok_or_else(|| anyhow::anyhow!("The model's answer holds no JSON object"))?;
    let end = text
        .rfind('}')
        .filter(|end| *end > start)
        .ok_or_else(|| anyhow::anyhow!("The model's answer holds no complete JSON object"))?;
    let raw: RawSummary = serde_json::from_str(&text[start..=end])
        .map_err(|e| anyhow::anyhow!("The model's answer is not the expected JSON: {e}"))?;
    Ok(ComponentSummary {
        summary: raw.summary,
        responsibility: raw.responsibility,
        constraints: raw.constraints,
        interfaces: raw
            .interfaces
            .into_iter()
            .map(|i| match i {
                RawInterface::Note(note) => note,
                RawInterface::Text(name) => InterfaceNote {
                    name,
                    description: String::new(),
                },
            })
            .collect(),
        parts: raw.parts,
        ..ComponentSummary::default()
    })
}

/// The cache key of a digest.
pub fn digest_key(digest: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(PROMPT_VERSION.as_bytes());
    hasher.update(digest.as_bytes());
    hasher
        .finalize()
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn clip(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        text
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

/// Items joined with `; ` until `budget` characters are used.
fn items_within(items: &[String], budget: usize) -> String {
    let mut out = String::new();
    for (ix, item) in items.iter().enumerate() {
        if out.len() + item.len() > budget {
            out.push_str(&format!(" (+{} more)", items.len() - ix));
            break;
        }
        if !out.is_empty() {
            out.push_str("; ");
        }
        out.push_str(item);
    }
    out
}

/// The digest of component `id`; `None` if it does not exist.
pub fn build_digest(model: &ArchitectureModel, id: &str) -> Option<String> {
    let component = model.get(id)?;
    let names = |ids: Vec<&str>| {
        ids.into_iter()
            .map(|id| model.name(id).to_owned())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut out = String::new();
    let project = model.name("");
    let path = if id.is_empty() { "." } else { id };
    out.push_str(&format!(
        "# Component: {} ({}{}), path {path}, in project {project}\n",
        component.name,
        component.kind.label(),
        component
            .language
            .map(|l| format!(", {}", l.label()))
            .unwrap_or_default(),
    ));
    out.push_str(&format!(
        "Size: {} files, {} lines\n",
        component.file_count, component.line_count
    ));
    if let Some(description) = &component.description {
        out.push_str(&format!("Description: {}\n", clip(description, 800)));
    }
    let uses = model.uses(id);
    if !uses.is_empty() {
        out.push_str(&format!("Uses sibling components: {}\n", names(uses)));
    }
    let used_by = model.used_by(id);
    if !used_by.is_empty() {
        out.push_str(&format!("Used by sibling components: {}\n", names(used_by)));
    }
    if !component.external_deps.is_empty() {
        let deps: Vec<String> = component.external_deps.iter().take(30).cloned().collect();
        out.push_str(&format!("Declared dependencies: {}\n", deps.join(", ")));
    }
    if !component.interface.is_empty() {
        out.push_str("Public interface:\n");
        for item in component.interface.iter().take(30) {
            out.push_str(&format!("- {item}\n"));
        }
    }

    let children = &component.children;
    if children.is_empty() {
        return Some(out);
    }
    out.push_str(&format!("\n# Parts ({})\n", children.len()));
    let described = children.len().min(MAX_PARTS);
    let per_part = (DIGEST_BUDGET.saturating_sub(out.len()) / described).max(240);
    for child_id in children.iter().take(described) {
        let Some(child) = model.get(child_id) else {
            continue;
        };
        let mut part = format!(
            "## {} ({}, {} files, {} lines)\n",
            child.name,
            child.kind.label(),
            child.file_count,
            child.line_count
        );
        if let Some(doc) = &child.description {
            part.push_str(&format!("Doc: {}\n", clip(doc, per_part / 3)));
        }
        let uses = model.uses(child_id);
        if !uses.is_empty() {
            part.push_str(&format!("Uses: {}\n", names(uses)));
        }
        if child.kind == ComponentKind::Package && !child.external_deps.is_empty() {
            let deps: Vec<String> = child.external_deps.iter().take(12).cloned().collect();
            part.push_str(&format!("Dependencies: {}\n", deps.join(", ")));
        }
        let mut exposed = child.interface.clone();
        if exposed.is_empty() {
            // A folder without entry items: what its parts expose.
            exposed = child
                .children
                .iter()
                .filter_map(|c| model.get(c))
                .flat_map(|c| c.interface.iter().take(3).cloned())
                .collect();
        }
        if !exposed.is_empty() {
            let budget = per_part.saturating_sub(part.len());
            part.push_str(&format!("Exposes: {}\n", items_within(&exposed, budget)));
        }
        out.push_str(&part);
    }
    if children.len() > described {
        let rest: Vec<&str> = children[described..]
            .iter()
            .map(|c| model.name(c))
            .collect();
        out.push_str(&format!("## Further parts: {}\n", rest.join(", ")));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_json_with_string_interfaces() {
        let text = "Here it is:\n```json\n{\"summary\": \"S\", \"responsibility\": \"R\", \
                    \"constraints\": [\"C\"], \"interfaces\": [\"Foo\", {\"name\": \"Bar\", \
                    \"description\": \"b\"}], \"parts\": {\"a\": \"does a\"}}\n```";
        let summary = parse_summary(text).unwrap();
        assert_eq!(summary.summary, "S");
        assert_eq!(summary.constraints, vec!["C"]);
        assert_eq!(summary.interfaces[0].name, "Foo");
        assert_eq!(summary.interfaces[1].description, "b");
        assert_eq!(summary.parts["a"], "does a");
        assert!(parse_summary("no json").is_err());
    }

    #[test]
    fn keys_differ_with_the_digest() {
        assert_eq!(digest_key("a"), digest_key("a"));
        assert_ne!(digest_key("a"), digest_key("b"));
        assert_eq!(digest_key("a").len(), 32);
    }
}
