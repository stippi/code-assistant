//! A `CompletionProvider` for the input composer that autocompletes skills.
//!
//! When the current line starts with `/`, this provider offers the
//! session's skills (read from the [`crate::Gpui`] global, populated via
//! `BackendEvent::ListSkills`). Selecting one replaces the typed `/...` with
//! `/<skill-name>`. On submit, [`super::InputArea::on_enter`] recognizes a
//! lone `/<skill-name>` that matches a known skill and translates it into a
//! skill invocation (see [`skill_invocation_from_input`]).

use std::time::Duration;

use anyhow::Result;
use code_assistant_core::session::service::SkillCatalogEntry;
use gpui::{Context, Task, Window};
use gpui_component::input::{InputState, RopeExt};
use gpui_component::Rope;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    TextEdit,
};

use crate::Gpui;

/// Completion provider that suggests skills after a leading `/`.
#[derive(Default)]
pub struct SkillCompletionProvider;

impl SkillCompletionProvider {
    pub fn new() -> Self {
        Self
    }
}

/// Extract the current line's text up to `offset` (cursor) as a string.
fn line_prefix(text: &Rope, offset: usize) -> (usize, String) {
    let full = text.to_string();
    let offset = offset.min(full.len());
    let line_start = full[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    (line_start, full[line_start..offset].to_string())
}

impl gpui_component::input::CompletionProvider for SkillCompletionProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        cx: &mut Context<InputState>,
    ) -> Task<Result<CompletionResponse>> {
        let (line_start, prefix) = line_prefix(text, offset);

        // Only offer skill completions on a line that begins with '/'.
        if !prefix.starts_with('/') {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        }

        // The query is the text after the leading '/'. Skill names are a single
        // token ([a-z0-9-]); anything with a space is not a skill query.
        let query = prefix[1..].to_lowercase();
        if query.contains(char::is_whitespace) {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        }

        let skills = cx.global::<Gpui>().skills();

        // Replace the whole typed `/...` token with `/<name>` on accept.
        let start = text.offset_to_position(line_start);
        let end = text.offset_to_position(offset);

        let items: Vec<CompletionItem> = skills
            .iter()
            .filter(|s| {
                query.is_empty()
                    || s.name.to_lowercase().contains(&query)
                    || s.description.to_lowercase().contains(&query)
            })
            .map(|s| CompletionItem {
                label: s.name.clone(),
                kind: Some(CompletionItemKind::SNIPPET),
                detail: Some(format!("({}) {}", s.scope_label, s.description)),
                filter_text: Some(s.name.clone()),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range: lsp_types::Range { start, end },
                    new_text: format!("/{}", s.name),
                })),
                ..Default::default()
            })
            .collect();

        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(
        &self,
        _offset: usize,
        _new_text: &str,
        _cx: &mut Context<InputState>,
    ) -> bool {
        // Be permissive: `completions` gates on the leading-'/' line prefix and
        // returns an empty list (which hides the menu) outside a slash context.
        true
    }

    fn inline_completion_debounce(&self) -> Duration {
        Duration::from_millis(0)
    }
}

/// What pressing Enter on the composer should do, given the current input and
/// the available skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashState {
    /// The input is a complete `/<skill-name>` — activate it.
    Invoke { scope: String, name: String },
    /// The input is a `/<query>` for which the completion menu is showing at
    /// least one entry. Enter should confirm the highlighted entry (handled by
    /// the inner input's completion menu), not submit the message.
    MenuOpen,
    /// Not a slash-completion context — submit as an ordinary message.
    None,
}

/// Whether a `/<query>` line would show at least one completion entry. Mirrors
/// the filter in [`SkillCompletionProvider::completions`] so callers can tell
/// when the completion menu is open.
fn query_matches(query: &str, skills: &[SkillCatalogEntry]) -> bool {
    let q = query.to_lowercase();
    skills.iter().any(|s| {
        q.is_empty()
            || s.name.to_lowercase().contains(&q)
            || s.description.to_lowercase().contains(&q)
    })
}

/// Classify the composer input for Enter handling (see [`SlashState`]).
pub fn slash_completion_state(input: &str, skills: &[SkillCatalogEntry]) -> SlashState {
    let trimmed = input.trim();
    let Some(rest) = trimmed.strip_prefix('/') else {
        return SlashState::None;
    };
    // A slash command is a single bare token (skill names never contain spaces).
    if rest.contains(char::is_whitespace) {
        return SlashState::None;
    }
    // A fully-typed, known skill name activates immediately.
    if let Some(s) = skills.iter().find(|s| s.name == rest) {
        return SlashState::Invoke {
            scope: s.scope_token.clone(),
            name: s.name.clone(),
        };
    }
    // Otherwise, if the menu is showing matches, Enter belongs to the menu.
    if query_matches(rest, skills) {
        SlashState::MenuOpen
    } else {
        SlashState::None
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
    fn line_prefix_extracts_current_line() {
        let rope = Rope::from("first line\n/sec");
        let (start, prefix) = line_prefix(&rope, rope.to_string().len());
        assert_eq!(prefix, "/sec");
        assert_eq!(start, "first line\n".len());
    }
}
