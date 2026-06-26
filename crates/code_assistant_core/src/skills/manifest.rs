//! Parsing and validation of `SKILL.md` files.
//!
//! A skill file starts with a YAML frontmatter block delimited by `---`,
//! followed by a free-form Markdown body. Only the fields needed by the
//! initial skills slice (`name`, `description`) are parsed today; unknown
//! frontmatter keys are tolerated so skills authored against the full
//! Anthropic spec still load.

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// Maximum length of a skill `name` (Anthropic Agent Skills spec).
const MAX_NAME_LEN: usize = 64;
/// Maximum length of a skill `description` (Anthropic Agent Skills spec).
const MAX_DESCRIPTION_LEN: usize = 1024;

/// The validated metadata extracted from a `SKILL.md` frontmatter block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillManifest {
    pub name: String,
    pub description: String,
}

/// Raw frontmatter shape. All fields are optional so deserialization never
/// hard-fails on a missing key; required fields are enforced afterwards in
/// [`parse_skill_content`]. Unknown keys are ignored on purpose.
#[derive(Debug, Default, Deserialize)]
struct SkillFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

/// Parse the contents of a `SKILL.md` file into its validated [`SkillManifest`]
/// and the Markdown body that follows the frontmatter.
pub fn parse_skill_content(content: &str) -> Result<(SkillManifest, String)> {
    let (frontmatter, body) = split_frontmatter(content)
        .context("SKILL.md must start with a YAML frontmatter block delimited by `---` lines")?;

    let parsed: SkillFrontmatter = serde_yaml_ng::from_str(&frontmatter)
        .context("Failed to parse SKILL.md YAML frontmatter")?;

    let name = parsed.name.unwrap_or_default().trim().to_string();
    let description = parsed.description.unwrap_or_default().trim().to_string();

    validate_name(&name)?;
    validate_description(&description)?;

    Ok((SkillManifest { name, description }, body))
}

/// Split a `SKILL.md` into its frontmatter and body.
///
/// The very first line must be exactly `---`; the frontmatter ends at the next
/// `---` line. Returns `None` when no well-formed frontmatter block is present.
fn split_frontmatter(content: &str) -> Option<(String, String)> {
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return None;
    }

    let mut frontmatter = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        frontmatter.push(line);
    }
    if !closed {
        return None;
    }

    let body = lines.collect::<Vec<_>>().join("\n");
    Some((frontmatter.join("\n"), body.trim_start().to_string()))
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("skill `name` is required and must not be empty");
    }
    if name.chars().count() > MAX_NAME_LEN {
        bail!("skill `name` exceeds the maximum length of {MAX_NAME_LEN} characters");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!("skill `name` may only contain lowercase letters, digits, and hyphens");
    }
    Ok(())
}

fn validate_description(description: &str) -> Result<()> {
    if description.is_empty() {
        bail!("skill `description` is required and must not be empty");
    }
    if description.chars().count() > MAX_DESCRIPTION_LEN {
        bail!("skill `description` exceeds the maximum length of {MAX_DESCRIPTION_LEN} characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_skill() {
        let content = "---\nname: pdf-extraction\ndescription: Extract text from PDFs.\n---\n\n# Body\n\nDo the thing.\n";
        let (manifest, body) = parse_skill_content(content).expect("should parse");
        assert_eq!(manifest.name, "pdf-extraction");
        assert_eq!(manifest.description, "Extract text from PDFs.");
        assert_eq!(body, "# Body\n\nDo the thing.");
    }

    #[test]
    fn parses_block_scalar_description() {
        let content =
            "---\nname: my-skill\ndescription: |-\n  A multi-line\n  description.\n---\nbody";
        let (manifest, _body) = parse_skill_content(content).expect("should parse");
        assert_eq!(manifest.description, "A multi-line\ndescription.");
    }

    #[test]
    fn tolerates_unknown_frontmatter_keys() {
        let content = "---\nname: my-skill\ndescription: ok\nlicense: Apache-2.0\nallowed-tools: read_files\n---\nbody";
        let (manifest, _body) = parse_skill_content(content).expect("should parse");
        assert_eq!(manifest.name, "my-skill");
    }

    #[test]
    fn rejects_missing_frontmatter() {
        let err = parse_skill_content("# Just markdown\n").unwrap_err();
        assert!(err.to_string().contains("frontmatter"));
    }

    #[test]
    fn rejects_unterminated_frontmatter() {
        let err = parse_skill_content("---\nname: my-skill\ndescription: ok\n").unwrap_err();
        assert!(err.to_string().contains("frontmatter"));
    }

    #[test]
    fn rejects_missing_name() {
        let err = parse_skill_content("---\ndescription: ok\n---\nbody").unwrap_err();
        assert!(err.to_string().contains("name"));
    }

    #[test]
    fn rejects_missing_description() {
        let err = parse_skill_content("---\nname: my-skill\n---\nbody").unwrap_err();
        assert!(err.to_string().contains("description"));
    }

    #[test]
    fn rejects_invalid_name_characters() {
        let err =
            parse_skill_content("---\nname: My_Skill\ndescription: ok\n---\nbody").unwrap_err();
        assert!(err.to_string().contains("name"));
    }

    #[test]
    fn rejects_oversize_name() {
        let long = "a".repeat(MAX_NAME_LEN + 1);
        let content = format!("---\nname: {long}\ndescription: ok\n---\nbody");
        let err = parse_skill_content(&content).unwrap_err();
        assert!(err.to_string().contains("name"));
    }
}
