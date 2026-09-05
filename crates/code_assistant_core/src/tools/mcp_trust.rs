//! Persistent per-project trust for project-local `.mcp.json` files.
//!
//! Loading a project's `.mcp.json` launches the MCP servers it names — i.e. it
//! runs commands supplied by the project. To avoid doing that for an untrusted
//! repository without the user's say-so, the session manager gates it behind a
//! permission prompt and records the approval here, keyed to the file's
//! contents. The record lives in `mcp-trust.json` in the config dir; a later
//! edit to the `.mcp.json` changes its fingerprint and re-prompts.

use crate::config_dir::config_dir;
use crate::utils::file_utils::atomic_write_json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const TRUST_FILE: &str = "mcp-trust.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustStore {
    /// Absolute project path → md5 fingerprint of the approved `.mcp.json`.
    #[serde(default)]
    trusted: BTreeMap<String, String>,
}

fn trust_path() -> PathBuf {
    config_dir().join(TRUST_FILE)
}

fn load() -> TrustStore {
    let path = trust_path();
    let Ok(content) = std::fs::read_to_string(&path) else {
        return TrustStore::default();
    };
    serde_json::from_str(&content).unwrap_or_else(|e| {
        tracing::warn!("Failed to parse {}: {e}", path.display());
        TrustStore::default()
    })
}

fn key(project_dir: &Path) -> String {
    // Canonicalize so the same project keyed via different spellings (trailing
    // slash, symlink, `..`) resolves to one trust entry and does not re-prompt.
    // Falls back to the path as-is when it cannot be canonicalized (e.g. it no
    // longer exists), preserving the previous behavior in that case.
    std::fs::canonicalize(project_dir)
        .unwrap_or_else(|_| project_dir.to_path_buf())
        .display()
        .to_string()
}

/// The change-detection fingerprint of a `.mcp.json`'s contents. Not
/// cryptographic — only used to notice edits and re-prompt.
pub fn fingerprint(content: &str) -> String {
    format!("{:x}", md5::compute(content))
}

/// Whether `project_dir`'s `.mcp.json`, at the given content fingerprint, has
/// been approved. An edited file (different fingerprint) is not trusted.
pub fn is_trusted(project_dir: &Path, fingerprint: &str) -> bool {
    load()
        .trusted
        .get(&key(project_dir))
        .is_some_and(|approved| approved == fingerprint)
}

/// Record approval of `project_dir`'s `.mcp.json` at the given fingerprint,
/// replacing any previous fingerprint recorded for that project.
pub fn add_trusted(project_dir: &Path, fingerprint: &str) -> anyhow::Result<()> {
    let mut store = load();
    store
        .trusted
        .insert(key(project_dir), fingerprint.to_string());
    atomic_write_json(&trust_path(), &store)
}

/// Whether `project_dir` has a `.mcp.json` that is not yet trusted — i.e.
/// loading it would require asking the user. Cheap and non-interactive; lets a
/// caller decide whether trust resolution must run off its critical path
/// (the interactive prompt must not block a single-threaded command worker).
pub fn needs_prompt(project_dir: &Path) -> bool {
    match std::fs::read_to_string(project_dir.join(".mcp.json")) {
        Ok(content) => !is_trusted(project_dir, &fingerprint(&content)),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_round_trip_and_fingerprint_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(dir.path()), || {
            let project = Path::new("/some/project");
            let fp_a = fingerprint("servers-a");
            assert!(!is_trusted(project, &fp_a), "unrecorded project untrusted");

            add_trusted(project, &fp_a).unwrap();
            assert!(is_trusted(project, &fp_a), "recorded fingerprint trusted");

            // An edited .mcp.json (different fingerprint) is not trusted.
            let fp_b = fingerprint("servers-b");
            assert!(!is_trusted(project, &fp_b), "changed contents re-prompt");

            // Re-approving replaces the fingerprint.
            add_trusted(project, &fp_b).unwrap();
            assert!(is_trusted(project, &fp_b));
            assert!(
                !is_trusted(project, &fp_a),
                "old fingerprint no longer trusted"
            );
        });
    }

    #[test]
    fn unknown_project_is_untrusted() {
        let dir = tempfile::tempdir().unwrap();
        temp_env::with_var("CODE_ASSISTANT_CONFIG_DIR", Some(dir.path()), || {
            assert!(!is_trusted(Path::new("/never/approved"), &fingerprint("x")));
        });
    }
}
