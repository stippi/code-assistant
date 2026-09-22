//! The composer's slash menu: which entries a `/<query>` offers and what Enter
//! does with it.
//!
//! When the input is a bare `/<query>`, the composer shows the session's
//! skills (read from the [`crate::Gpui`] global, populated via
//! `BackendEvent::ListSkills`) plus the built-in `/goal` entry. Accepting an
//! entry replaces the input with `/<skill-name>`; `/goal` expands to the
//! required `/goal ` template. On submit, [`super::InputArea::on_enter`]
//! recognizes a lone `/<skill-name>` that matches a known skill and translates
//! it into a skill invocation (see [`slash_completion_state`]).
//!
//! Until the gpui-kit migration the menu was the input component's LSP
//! completion popover; gpui-kit offers completions on its code editor only, so
//! the composer (a textarea) renders the menu itself.

use code_assistant_core::session::service::SkillCatalogEntry;

const GOAL_DESCRIPTION: &str = "Set completion criteria, or type /goal cancel";

/// One entry of the slash menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashMenuItem {
    /// The name shown first, e.g. `goal` or the skill's name.
    pub label: String,
    /// The muted text after the label.
    pub detail: String,
    /// What replaces the input when the entry is accepted.
    pub insert: String,
}

/// The bare `/<token>` of a slash-command input, if the input is one.
///
/// A slash command is a single token (skill names never contain spaces). A
/// space after the token ends the query: `/goal ` is the accepted template
/// waiting for its criteria, not a query for `goal`.
fn slash_query(input: &str) -> Option<&str> {
    let rest = input.trim_start().strip_prefix('/')?;
    (!rest.contains(char::is_whitespace)).then_some(rest)
}

/// The entries the slash menu shows for `input`; empty when the input is not a
/// `/<query>` or nothing matches.
pub fn slash_menu_items(input: &str, skills: &[SkillCatalogEntry]) -> Vec<SlashMenuItem> {
    let Some(query) = slash_query(input) else {
        return Vec::new();
    };
    let query = query.to_lowercase();
    let mut items = Vec::new();
    if query.is_empty()
        || "goal".contains(&query)
        || GOAL_DESCRIPTION.to_lowercase().contains(&query)
    {
        items.push(SlashMenuItem {
            label: "goal".into(),
            detail: GOAL_DESCRIPTION.into(),
            insert: "/goal ".into(),
        });
    }
    items.extend(
        skills
            .iter()
            .filter(|s| {
                query.is_empty()
                    || s.name.to_lowercase().contains(&query)
                    || s.description.to_lowercase().contains(&query)
            })
            .map(|s| SlashMenuItem {
                label: s.name.clone(),
                detail: format!("({}) {}", s.scope_label, s.description),
                insert: format!("/{}", s.name),
            }),
    );
    items
}

/// What pressing Enter on the composer should do, given the current input and
/// the available skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashState {
    /// The input is a complete `/<skill-name>` — activate it.
    Invoke { scope: String, name: String },
    /// The input is a `/<query>` for which the slash menu shows at least one
    /// entry. Enter should accept the highlighted entry, not submit.
    MenuOpen,
    /// Not a slash-completion context — submit as an ordinary message.
    None,
}

/// Classify the composer input for Enter handling (see [`SlashState`]).
pub fn slash_completion_state(input: &str, skills: &[SkillCatalogEntry]) -> SlashState {
    // Trailing whitespace does not keep a complete skill name from invoking.
    let Some(rest) = slash_query(input.trim_end()) else {
        return SlashState::None;
    };
    // A fully-typed, known skill name activates immediately.
    if let Some(s) = skills.iter().find(|s| s.name == rest) {
        return SlashState::Invoke {
            scope: s.scope_token.clone(),
            name: s.name.clone(),
        };
    }
    // Otherwise, if the menu is showing matches, Enter belongs to the menu.
    if slash_menu_items(input, skills).is_empty() {
        SlashState::None
    } else {
        SlashState::MenuOpen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, scope_token: &str) -> SkillCatalogEntry {
        SkillCatalogEntry {
            name: name.to_string(),
            description: "desc".to_string(),
            scope_token: scope_token.to_string(),
            scope_label: "project".to_string(),
        }
    }

    fn labels(items: &[SlashMenuItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn exact_name_invokes() {
        let skills = vec![entry("pdf-extraction", "proj"), entry("review", ":config:")];
        assert_eq!(
            slash_completion_state("/review", &skills),
            SlashState::Invoke {
                scope: ":config:".to_string(),
                name: "review".to_string()
            }
        );
        assert_eq!(
            slash_completion_state("  /pdf-extraction  ", &skills),
            SlashState::Invoke {
                scope: "proj".to_string(),
                name: "pdf-extraction".to_string()
            }
        );
    }

    #[test]
    fn prefix_query_keeps_menu_open() {
        let skills = vec![entry("pdf-extraction", "proj"), entry("review", ":config:")];
        // A bare slash shows all skills.
        assert_eq!(slash_completion_state("/", &skills), SlashState::MenuOpen);
        // A partial token that matches at least one skill keeps the menu open.
        assert_eq!(slash_completion_state("/pd", &skills), SlashState::MenuOpen);
        assert_eq!(
            slash_completion_state("/rev", &skills),
            SlashState::MenuOpen
        );
        assert_eq!(slash_completion_state("/go", &[]), SlashState::MenuOpen);
        assert_eq!(slash_completion_state("/goal", &[]), SlashState::MenuOpen);
        // The accepted `/goal ` template is a message once its criteria follow;
        // Enter on it must not re-accept the template forever.
        assert_eq!(slash_completion_state("/goal ", &[]), SlashState::None);
    }

    #[test]
    fn ignores_non_skill_input() {
        let skills = vec![entry("pdf-extraction", "proj")];
        // Ordinary message.
        assert_eq!(
            slash_completion_state("hello there", &skills),
            SlashState::None
        );
        // Slash but no matching skill → submit as a normal message.
        assert_eq!(slash_completion_state("/zzz", &skills), SlashState::None);
        // Slash with trailing text is not a bare skill token.
        assert_eq!(
            slash_completion_state("/pdf-extraction now", &skills),
            SlashState::None
        );
    }

    #[test]
    fn bare_slash_lists_goal_and_every_skill() {
        let skills = vec![entry("pdf-extraction", "proj"), entry("review", ":config:")];
        let items = slash_menu_items("/", &skills);
        assert_eq!(labels(&items), ["goal", "pdf-extraction", "review"]);
        assert_eq!(items[0].insert, "/goal ");
        assert_eq!(items[1].insert, "/pdf-extraction");
        assert_eq!(items[1].detail, "(project) desc");
    }

    #[test]
    fn query_filters_by_name_and_description() {
        let skills = vec![entry("pdf-extraction", "proj"), entry("review", ":config:")];
        assert_eq!(
            labels(&slash_menu_items("/PDF", &skills)),
            ["pdf-extraction"]
        );
        // "desc" is every test skill's description; "goal" does not match it.
        assert_eq!(
            labels(&slash_menu_items("/desc", &skills)),
            ["pdf-extraction", "review"]
        );
        assert_eq!(labels(&slash_menu_items("/crit", &skills)), ["goal"]);
        assert!(slash_menu_items("/zzz", &skills).is_empty());
    }

    #[test]
    fn menu_is_empty_outside_a_slash_command() {
        let skills = vec![entry("review", ":config:")];
        assert!(slash_menu_items("hello", &skills).is_empty());
        assert!(slash_menu_items("/review now", &skills).is_empty());
        assert!(slash_menu_items("/goal ", &skills).is_empty());
        assert!(slash_menu_items("", &skills).is_empty());
    }
}
