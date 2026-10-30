//! What the pull request view remembers across restarts: how the list is
//! sorted and filtered, the merge method last used per repository, and how
//! the Code tab shows diffs.
//! Kept with the other UI settings ([`UiSettings`](crate::shared::settings::UiSettings)).

use super::list_logic::{Filters, SortMode};
use github::RepoRef;
use github::model::{Involvement, ListState, MergeMethod};
use gpui_kit::App;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PullRequestPreferences {
    pub list: ListPreferences,
    /// The merge method last used, by repository (`host/owner/name`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub merge_methods: BTreeMap<String, MergeMethod>,
    /// The Summary's comment filter and order, the Timeline's order.
    pub conversation: super::conversation::preferences::ConversationPreferences,
    pub code: CodePreferences,
}

/// Stacked rows (old and new lines one below the other) or split (side
/// by side).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffLayout {
    #[default]
    Stacked,
    Split,
}

/// How the Code tab shows diffs. T3 Code takes the defaults from its
/// settings (lines wrap, whitespace changes hidden); here the toggles
/// themselves are remembered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodePreferences {
    pub layout: DiffLayout,
    pub wrap: bool,
    pub hide_whitespace: bool,
    pub hide_tests: bool,
    pub file_tree: bool,
}

impl Default for CodePreferences {
    fn default() -> Self {
        Self {
            layout: DiffLayout::Stacked,
            wrap: true,
            hide_whitespace: true,
            hide_tests: false,
            file_tree: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ListPreferences {
    pub sort: SortMode,
    pub filters: Filters,
    pub state: ListState,
    pub involvement: Involvement,
}

impl PullRequestPreferences {
    pub fn merge_method(&self, repo: &RepoRef) -> Option<MergeMethod> {
        self.merge_methods.get(&repo.to_string()).copied()
    }
}

/// The remembered preferences; the defaults before settings are loaded.
pub fn read(cx: &App) -> PullRequestPreferences {
    cx.try_global::<crate::UiSettingsGlobal>()
        .map(|settings| settings.0.pull_requests.clone())
        .unwrap_or_default()
}

/// Change the preferences; they are saved with the UI settings.
pub fn update(cx: &mut App, change: impl FnOnce(&mut PullRequestPreferences)) {
    crate::update_ui_settings(cx, |settings| change(&mut settings.pull_requests));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::main_screen::right_panel::pull_request::fixtures::repo;
    use crate::main_screen::right_panel::pull_request::list_logic::DraftFilter;

    #[test]
    fn preferences_survive_a_round_trip_and_default_when_missing() {
        let mut preferences = PullRequestPreferences::default();
        preferences.list.sort = SortMode::Newest;
        preferences.list.state = ListState::Merged;
        preferences.list.filters.draft = Some(DraftFilter::Hide);
        preferences.code.layout = DiffLayout::Split;
        preferences.code.wrap = false;
        preferences
            .merge_methods
            .insert(repo().to_string(), MergeMethod::Squash);
        let json = serde_json::to_string(&preferences).unwrap();
        assert_eq!(
            serde_json::from_str::<PullRequestPreferences>(&json).unwrap(),
            preferences
        );
        assert_eq!(preferences.merge_method(&repo()), Some(MergeMethod::Squash));
        assert_eq!(
            serde_json::from_str::<PullRequestPreferences>("{}").unwrap(),
            PullRequestPreferences::default()
        );
    }
}
