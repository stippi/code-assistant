//! Installed plugins: where they come from, where they live, whether they
//! are on.
//!
//! ```text
//! <config_dir>/plugins/
//!   plugins.json        # the installed plugins (this module's record)
//!   <name>/             # the clone of a plugin installed from git
//!   .staging/           # clones being checked before they replace one
//! ```
//!
//! A plugin from git is cloned (shallow, through the user's own `git`, so
//! SSH keys and credential helpers work for private repositories) and its
//! commit recorded; updating clones again and swaps the folder once the new
//! manifest checks out. A local folder is linked, not copied: edits to it
//! show at once, which is what developing a plugin wants.

use super::manifest::{PluginManifest, is_valid_name};
use crate::utils::file_utils::atomic_write_json;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const RECORD_FILE: &str = "plugins.json";
const STAGING_DIR: &str = ".staging";

/// The record in `plugins.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginRecord {
    #[serde(default)]
    pub plugins: BTreeMap<String, InstalledPlugin>,
}

/// One installed plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub source: PluginSource,
    /// The plugin's root folder: its clone, or the linked folder.
    pub path: PathBuf,
    /// The commit a git plugin was installed or last updated at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Where a plugin comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum PluginSource {
    /// A git repository, at a branch, tag or commit (`git_ref`) or its
    /// default branch.
    Git {
        url: String,
        #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
        git_ref: Option<String>,
    },
    /// A folder on this machine, used in place.
    Local { path: PathBuf },
}

impl PluginSource {
    /// Read what the user typed: an existing folder, a git URL
    /// (`https://…`, `ssh://…`, `git@host:…`, `file://…`) or GitHub's
    /// `owner/repo`, each git form optionally followed by `#ref`.
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            bail!("no plugin source given");
        }
        let expanded = expand_home(input);
        if expanded.is_dir() {
            let path = std::fs::canonicalize(&expanded).unwrap_or(expanded);
            return Ok(Self::Local { path });
        }
        let (location, git_ref) = match input.rsplit_once('#') {
            Some((location, git_ref)) if !git_ref.is_empty() => {
                (location, Some(git_ref.to_string()))
            }
            _ => (input, None),
        };
        let url = if location.contains("://") || location.starts_with("git@") {
            location.to_string()
        } else if is_github_shorthand(location) {
            format!("https://github.com/{location}.git")
        } else {
            bail!("'{input}' is neither a folder nor a git repository URL");
        };
        Ok(Self::Git { url, git_ref })
    }

    /// How the source reads in lists.
    pub fn display(&self) -> String {
        match self {
            Self::Git { url, git_ref: None } => url.clone(),
            Self::Git {
                url,
                git_ref: Some(git_ref),
            } => format!("{url}#{git_ref}"),
            Self::Local { path } => path.display().to_string(),
        }
    }
}

fn expand_home(input: &str) -> PathBuf {
    match input.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(input)),
        None => PathBuf::from(input),
    }
}

fn is_github_shorthand(location: &str) -> bool {
    let mut parts = location.split('/');
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    matches!((parts.next(), parts.next(), parts.next()), (Some(owner), Some(repo), None) if valid(owner) && valid(repo))
}

/// A plugin as installed, with its manifest (or why it cannot be read).
#[derive(Debug, Clone)]
pub struct PluginListing {
    pub name: String,
    pub installed: InstalledPlugin,
    pub manifest: Result<PluginManifest, String>,
}

/// The plugins folder and its record.
#[derive(Debug, Clone)]
pub struct PluginStore {
    dir: PathBuf,
}

impl PluginStore {
    /// The store in the configuration directory.
    pub fn new() -> Self {
        Self::at(crate::config_dir::config_dir().join("plugins"))
    }

    /// A store in `dir` (tests, other embedders).
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn record_path(&self) -> PathBuf {
        self.dir.join(RECORD_FILE)
    }

    /// The record; empty when there is none or it cannot be read.
    pub fn record(&self) -> PluginRecord {
        let path = self.record_path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return PluginRecord::default();
        };
        serde_json::from_str(&content).unwrap_or_else(|e| {
            tracing::warn!("Failed to parse {}: {e}", path.display());
            PluginRecord::default()
        })
    }

    fn save(&self, record: &PluginRecord) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        atomic_write_json(&self.record_path(), record)?;
        super::catalog::invalidate();
        Ok(())
    }

    /// Every installed plugin with its manifest.
    pub fn list(&self) -> Vec<PluginListing> {
        self.record()
            .plugins
            .into_iter()
            .map(|(name, installed)| {
                let manifest = PluginManifest::load(&installed.path).map_err(|e| format!("{e:#}"));
                PluginListing {
                    name,
                    installed,
                    manifest,
                }
            })
            .collect()
    }

    /// Install the plugin at `source` (see [`PluginSource::parse`]). Fails
    /// when its manifest is invalid or a plugin of its name is installed.
    pub fn install(&self, source: &str) -> Result<PluginListing> {
        let source = PluginSource::parse(source)?;
        let (path, manifest, commit) = match &source {
            PluginSource::Local { path } => (path.clone(), PluginManifest::load(path)?, None),
            PluginSource::Git { url, git_ref } => {
                let staged = self.clone_staged(url, git_ref.as_deref())?;
                let manifest = match PluginManifest::load(&staged) {
                    Ok(manifest) => manifest,
                    Err(e) => {
                        let _ = std::fs::remove_dir_all(&staged);
                        return Err(e);
                    }
                };
                if self.record().plugins.contains_key(&manifest.name) {
                    let _ = std::fs::remove_dir_all(&staged);
                    bail!("a plugin named '{}' is installed already", manifest.name);
                }
                let commit = head_commit(&staged);
                let target = self.dir.join(&manifest.name);
                replace_dir(&staged, &target)?;
                (target, manifest, commit)
            }
        };
        let mut record = self.record();
        if record.plugins.contains_key(&manifest.name) {
            bail!("a plugin named '{}' is installed already", manifest.name);
        }
        let installed = InstalledPlugin {
            source,
            path,
            commit,
            enabled: true,
        };
        record
            .plugins
            .insert(manifest.name.clone(), installed.clone());
        self.save(&record)?;
        tracing::info!("Installed plugin '{}'", manifest.name);
        Ok(PluginListing {
            name: manifest.name.clone(),
            installed,
            manifest: Ok(manifest),
        })
    }

    /// Fetch plugin `name` again from its git source; a linked folder only
    /// has its manifest checked. The clone is swapped only once the new
    /// manifest checks out and still carries the same name.
    pub fn update(&self, name: &str) -> Result<PluginListing> {
        let mut record = self.record();
        let installed = record
            .plugins
            .get_mut(name)
            .ok_or_else(|| anyhow!("no plugin named '{name}' is installed"))?;
        let manifest = match installed.source.clone() {
            PluginSource::Local { path } => PluginManifest::load(&path)?,
            PluginSource::Git { url, git_ref } => {
                let staged = self.clone_staged(&url, git_ref.as_deref())?;
                let manifest = PluginManifest::load(&staged).and_then(|manifest| {
                    if manifest.name == name {
                        Ok(manifest)
                    } else {
                        Err(anyhow!(
                            "the repository now holds plugin '{}', not '{name}'",
                            manifest.name
                        ))
                    }
                });
                let manifest = match manifest {
                    Ok(manifest) => manifest,
                    Err(e) => {
                        let _ = std::fs::remove_dir_all(&staged);
                        return Err(e);
                    }
                };
                installed.commit = head_commit(&staged);
                replace_dir(&staged, &installed.path)?;
                manifest
            }
        };
        let installed = installed.clone();
        self.save(&record)?;
        Ok(PluginListing {
            name: name.to_string(),
            installed,
            manifest: Ok(manifest),
        })
    }

    /// Uninstall plugin `name`; its clone is deleted, a linked folder kept.
    pub fn remove(&self, name: &str) -> Result<()> {
        let mut record = self.record();
        let installed = record
            .plugins
            .remove(name)
            .ok_or_else(|| anyhow!("no plugin named '{name}' is installed"))?;
        if matches!(installed.source, PluginSource::Git { .. })
            && installed.path.starts_with(&self.dir)
        {
            std::fs::remove_dir_all(&installed.path)
                .with_context(|| format!("removing {}", installed.path.display()))?;
        }
        self.save(&record)
    }

    /// Switch plugin `name` on or off.
    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        let mut record = self.record();
        let installed = record
            .plugins
            .get_mut(name)
            .ok_or_else(|| anyhow!("no plugin named '{name}' is installed"))?;
        installed.enabled = enabled;
        self.save(&record)
    }

    /// Clone `url` (at `git_ref`) into a fresh staging folder.
    fn clone_staged(&self, url: &str, git_ref: Option<&str>) -> Result<PathBuf> {
        let staging = self.dir.join(STAGING_DIR);
        std::fs::create_dir_all(&staging)?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let target = staging.join(format!("{}-{nanos}", std::process::id()));
        let result = match git_ref {
            // A branch or tag clones shallow; a commit needs a fetch by id.
            Some(git_ref) if looks_like_commit(git_ref) => {
                run_git(&["clone", "--no-checkout", "--", url], Some(&target))
                    .and_then(|()| run_git_in(&target, &["checkout", "--detach", git_ref]))
            }
            Some(git_ref) => run_git(
                &["clone", "--depth", "1", "--branch", git_ref, "--", url],
                Some(&target),
            ),
            None => run_git(&["clone", "--depth", "1", "--", url], Some(&target)),
        };
        if let Err(e) = result {
            let _ = std::fs::remove_dir_all(&target);
            return Err(e.context(format!("cloning {url}")));
        }
        Ok(target)
    }
}

impl Default for PluginStore {
    fn default() -> Self {
        Self::new()
    }
}

fn looks_like_commit(git_ref: &str) -> bool {
    (7..=40).contains(&git_ref.len()) && git_ref.chars().all(|c| c.is_ascii_hexdigit())
}

/// Run `git args… [target]` without prompting for credentials.
fn run_git(args: &[&str], target: Option<&Path>) -> Result<()> {
    let mut command = Command::new("git");
    command.args(args).env("GIT_TERMINAL_PROMPT", "0");
    if let Some(target) = target {
        command.arg(target);
    }
    let output = command
        .output()
        .context("could not run git (is it installed?)")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

fn run_git_in(dir: &Path, args: &[&str]) -> Result<()> {
    let mut all = vec!["-C", dir.to_str().unwrap_or_default()];
    all.extend_from_slice(args);
    run_git(&all, None)
}

fn head_commit(dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", dir.to_str()?, "rev-parse", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Move `from` to `to`, replacing what is there.
fn replace_dir(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        std::fs::remove_dir_all(to).with_context(|| format!("removing {}", to.display()))?;
    }
    std::fs::rename(from, to)
        .with_context(|| format!("moving {} to {}", from.display(), to.display()))
}

/// Whether `name` could be a plugin name (for CLI arguments).
pub fn check_name(name: &str) -> Result<()> {
    if is_valid_name(name) {
        Ok(())
    } else {
        bail!("'{name}' is not a plugin name")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_host::manifest::MANIFEST_PATH;

    fn write_plugin(dir: &Path, name: &str, description: &str) {
        let manifest = dir.join(MANIFEST_PATH);
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(
            manifest,
            format!(r#"{{ "name": "{name}", "description": "{description}" }}"#),
        )
        .unwrap();
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(status.status.success(), "git {args:?}: {status:?}");
    }

    /// A git repository holding plugin `name`, as a `file://` URL.
    fn plugin_repo(dir: &Path, name: &str) -> String {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
        write_plugin(dir, name, "first");
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "first"]);
        format!("file://{}", dir.display())
    }

    #[test]
    fn sources_are_read_as_folders_urls_or_shorthands() {
        let folder = tempfile::tempdir().unwrap();
        assert!(matches!(
            PluginSource::parse(folder.path().to_str().unwrap()).unwrap(),
            PluginSource::Local { .. }
        ));
        assert_eq!(
            PluginSource::parse("acme/diagrams#v1").unwrap(),
            PluginSource::Git {
                url: "https://github.com/acme/diagrams.git".into(),
                git_ref: Some("v1".into())
            }
        );
        assert_eq!(
            PluginSource::parse("git@github.com:acme/x.git").unwrap(),
            PluginSource::Git {
                url: "git@github.com:acme/x.git".into(),
                git_ref: None
            }
        );
        assert!(PluginSource::parse("not a source").is_err());
        assert!(PluginSource::parse("/does/not/exist").is_err());
    }

    #[test]
    fn a_git_plugin_installs_updates_and_uninstalls() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let url = plugin_repo(&repo, "demo");
        let store = PluginStore::at(temp.path().join("plugins"));

        let listing = store.install(&url).unwrap();
        assert_eq!(listing.name, "demo");
        let first = listing.installed.commit.clone().unwrap();
        assert_eq!(listing.installed.path, store.dir().join("demo"));
        assert!(store.dir().join("demo").join(MANIFEST_PATH).exists());
        assert!(store.install(&url).is_err(), "a second install of the name");

        write_plugin(&repo, "demo", "second");
        git(&repo, &["commit", "-q", "-am", "second"]);
        let updated = store.update("demo").unwrap();
        assert_ne!(updated.installed.commit.unwrap(), first);
        assert_eq!(
            updated.manifest.unwrap().description.as_deref(),
            Some("second")
        );

        store.set_enabled("demo", false).unwrap();
        assert!(!store.record().plugins["demo"].enabled);
        store.remove("demo").unwrap();
        assert!(store.record().plugins.is_empty());
        assert!(!store.dir().join("demo").exists());
        assert!(
            std::fs::read_dir(store.dir().join(STAGING_DIR))
                .unwrap()
                .next()
                .is_none(),
            "no staging leftovers"
        );
    }

    #[test]
    fn a_repository_without_a_valid_manifest_is_not_installed() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let url = plugin_repo(&repo, "Bad Name");
        let store = PluginStore::at(temp.path().join("plugins"));
        assert!(store.install(&url).is_err());
        assert!(store.record().plugins.is_empty());
        assert!(!store.dir().join("Bad Name").exists());
    }

    #[test]
    fn a_local_folder_is_linked_and_kept_on_removal() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("dev-plugin");
        write_plugin(&folder, "dev", "local");
        let store = PluginStore::at(temp.path().join("plugins"));
        let listing = store.install(folder.to_str().unwrap()).unwrap();
        assert_eq!(
            listing.installed.path,
            std::fs::canonicalize(&folder).unwrap()
        );
        store.remove("dev").unwrap();
        assert!(folder.join(MANIFEST_PATH).exists());
    }
}
