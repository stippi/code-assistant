//! How the code views (a pull request's Code tab, the Review tab) show
//! diffs. Kept with the pull request preferences in the UI settings.

use gpui_kit::App;
use serde::{Deserialize, Serialize};

/// Stacked rows (old and new lines one below the other) or split (side
/// by side).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffLayout {
    #[default]
    Stacked,
    Split,
}

/// How the code views show diffs. T3 Code takes the defaults from its
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

/// The remembered preferences; the defaults before settings are loaded.
pub fn read(cx: &App) -> CodePreferences {
    cx.try_global::<crate::UiSettingsGlobal>()
        .map(|settings| settings.0.pull_requests.code)
        .unwrap_or_default()
}

/// Change the preferences; they are saved with the UI settings.
pub fn update(cx: &mut App, change: impl FnOnce(&mut CodePreferences)) {
    crate::update_ui_settings(cx, |settings| change(&mut settings.pull_requests.code));
}
