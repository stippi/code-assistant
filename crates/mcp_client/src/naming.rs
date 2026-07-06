//! Registry names for MCP tools.
//!
//! Every MCP tool is registered as `mcp__<server>__<tool>` — the same
//! convention codex and Claude Code use. Names are sanitized to
//! `[A-Za-z0-9_-]` (the character set native tool calling accepts) and
//! capped at 64 bytes with a deterministic hash suffix, so they stay valid
//! across sessions and providers.

/// Maximum length native tool calling accepts for a tool name.
pub const MAX_TOOL_NAME_LENGTH: usize = 64;

/// Prefix marking registry tools that proxy an MCP server tool.
pub const MCP_TOOL_PREFIX: &str = "mcp__";

/// The registry name for a tool offered by a server.
pub fn registry_tool_name(server: &str, tool: &str) -> String {
    let name = format!("{MCP_TOOL_PREFIX}{}__{}", sanitize(server), sanitize(tool));
    if name.len() <= MAX_TOOL_NAME_LENGTH {
        return name;
    }
    // Keep names deterministic and distinct: truncate, then replace the tail
    // with a hash of the full (untruncated) name.
    let hash = format!("_{:08x}", fnv1a(name.as_bytes()));
    let mut prefix = name;
    prefix.truncate(MAX_TOOL_NAME_LENGTH - hash.len());
    format!("{prefix}{hash}")
}

pub(crate) fn sanitize(part: &str) -> String {
    part.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_names_pass_through() {
        assert_eq!(
            registry_tool_name("jira", "search_issues"),
            "mcp__jira__search_issues"
        );
    }

    #[test]
    fn invalid_characters_are_sanitized() {
        assert_eq!(
            registry_tool_name("my server", "search.issues"),
            "mcp__my_server__search_issues"
        );
    }

    #[test]
    fn long_names_are_capped_at_64_with_distinct_suffixes() {
        let long_a = "a".repeat(80);
        let long_b = format!("{}b", "a".repeat(79));
        let name_a = registry_tool_name("server", &long_a);
        let name_b = registry_tool_name("server", &long_b);
        assert_eq!(name_a.len(), MAX_TOOL_NAME_LENGTH);
        assert_eq!(name_b.len(), MAX_TOOL_NAME_LENGTH);
        assert_ne!(name_a, name_b, "truncated names must stay distinct");
        assert!(name_a.starts_with("mcp__server__"));
    }

    #[test]
    fn capping_is_deterministic() {
        let long = "tool_with_a_really_long_name_".repeat(4);
        assert_eq!(
            registry_tool_name("server", &long),
            registry_tool_name("server", &long)
        );
    }
}
