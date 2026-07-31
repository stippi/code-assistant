//! Central configuration directory resolution.
//!
//! All config files (models.json, providers.json, tools.json, projects.json,
//! ui-settings.json) live in a single directory determined by the following
//! priority:
//!
//! 1. `--config-dir` CLI argument (sets `CODE_ASSISTANT_CONFIG_DIR` env var)
//! 2. `CODE_ASSISTANT_CONFIG_DIR` environment variable
//! 3. `$XDG_CONFIG_HOME/code-assistant`
//! 4. `~/.config/code-assistant`

use std::path::PathBuf;

/// Returns the canonical configuration directory.
///
/// This is the single source of truth for where config files live.
pub fn config_dir() -> PathBuf {
    if let Ok(custom_dir) = std::env::var("CODE_ASSISTANT_CONFIG_DIR") {
        return PathBuf::from(custom_dir);
    }
    if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg_config).join("code-assistant");
    }
    if let Some(home_dir) = dirs::home_dir() {
        return home_dir.join(".config").join("code-assistant");
    }
    // Last resort fallback
    PathBuf::from("code-assistant")
}

/// Apply the `--config-dir` override by setting the environment variable.
///
/// Must be called early in main, before any config loading happens.
/// The env var is picked up by all config resolution code (including the `llm` crate).
pub fn apply_override(path: &PathBuf) {
    // SAFETY: `set_var` is unsafe on edition 2024 because concurrent env
    // access from other threads is UB. This is the documented `--config-dir`
    // entry point, called exactly once at the very start of `main` (see
    // `code_assistant::main`) before any threads, the tokio runtime, or any
    // config loading are spawned, so no other thread can be reading the
    // environment concurrently.
    unsafe {
        std::env::set_var("CODE_ASSISTANT_CONFIG_DIR", path);
    }
}
