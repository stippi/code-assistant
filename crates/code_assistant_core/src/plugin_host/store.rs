//! Installed plugins: where they come from, where they live, whether they
//! are on.
//!
//! ```text
//! <config_dir>/plugins/
//!   plugins.json        # the installed plugins (this module's record)
//!   marketplaces.json   # the added marketplaces
//!   <name>/             # the clone of a plugin installed from git
//!   .marketplaces/<m>/  # the clone of a marketplace added from git
//!   .staging/           # clones being checked before they replace one
//! ```
//!
//! A plugin from git is cloned (shallow, through the user's own `git`, so
//! SSH keys and credential helpers work for private repositories) and its
//! commit recorded; updating clones again and swaps the folder once the new
//! manifest checks out. A local folder is linked, not copied: edits to it
//! show at once, which is what developing a plugin wants.
//!
//! A marketplace ([`super::marketplace`]) is cloned or linked the same way;
//! its plugins install as `<plugin>@<marketplace>` from folders of that one
//! clone and share its commit, so updating the marketplace updates them.
//! [`PluginStore::add`] takes either: it looks at what the source holds.
//!
//! A repository that needs a login fails with [`GitLoginRequired`], which
//! says how to give git one; git's credential helpers and SSH agent are
//! what log in.

use super::manifest::{PluginManifest, is_valid_name};
use super::marketplace::MarketplaceManifest;
use crate::utils::file_utils::atomic_write_json;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const RECORD_FILE: &str = "plugins.json";
const MARKETPLACES_FILE: &str = "marketplaces.json";
const MARKETPLACES_DIR: &str = ".marketplaces";
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
    /// A folder of an added marketplace (`<plugin>@<marketplace>`); the
    /// plugin's name is its key in the record.
    Marketplace { marketplace: String },
}

impl PluginSource {
    /// Read what the user typed: an existing folder, a git URL
    /// (`https://…`, `ssh://…`, `git@host:…`, `file://…`) or GitHub's
    /// `owner/repo`, each git form optionally followed by `#ref`. (A
    /// marketplace's plugin, `<plugin>@<marketplace>`, is read by
    /// [`parse_marketplace_plugin`].)
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
            Self::Marketplace { marketplace } => format!("marketplace {marketplace}"),
        }
    }

    /// Whether the plugin is fetched with git (itself or its marketplace).
    pub fn is_remote(&self) -> bool {
        !matches!(self, Self::Local { .. })
    }
}

/// `<plugin>@<marketplace>`, both valid names, or `None`.
pub fn parse_marketplace_plugin(input: &str) -> Option<(&str, &str)> {
    let (plugin, marketplace) = input.trim().split_once('@')?;
    (is_valid_name(plugin) && is_valid_name(marketplace)).then_some((plugin, marketplace))
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

/// The record in `marketplaces.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MarketplaceRecord {
    #[serde(default)]
    pub marketplaces: BTreeMap<String, InstalledMarketplace>,
}

/// One added marketplace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstalledMarketplace {
    /// A git repository or a linked folder (never a marketplace).
    pub source: PluginSource,
    /// The marketplace's root folder: its clone, or the linked folder.
    pub path: PathBuf,
    /// The commit a git marketplace was added or last updated at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// A marketplace as added, with its manifest (or why it cannot be read).
#[derive(Debug, Clone)]
pub struct MarketplaceListing {
    pub name: String,
    pub installed: InstalledMarketplace,
    pub manifest: Result<MarketplaceManifest, String>,
}

/// What [`PluginStore::add`] added.
#[derive(Debug, Clone)]
pub enum Added {
    Plugin(PluginListing),
    Marketplace(MarketplaceListing),
}

/// A git repository refused the login, or there was none to give. Git logs
/// in with the user's credential helpers (HTTPS) or SSH agent; the app
/// never asks for or keeps passwords. Find it with
/// `error.downcast_ref::<GitLoginRequired>()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitLoginRequired {
    pub url: String,
    /// What git said.
    pub detail: String,
}

impl GitLoginRequired {
    /// Whether the repository is reached over SSH.
    pub fn is_ssh(&self) -> bool {
        self.url.starts_with("ssh://") || self.url.starts_with("git@")
    }

    /// How to give git a login for the repository.
    pub fn hint(&self) -> &'static str {
        if self.is_ssh() {
            "Load a key the server accepts into your SSH agent (ssh-add), then try again."
        } else {
            "Log in once with git in a terminal (git clone, or gh auth setup-git for GitHub) \
             so a credential helper keeps the login, or use the repository's SSH URL; \
             then try again."
        }
    }
}

impl std::fmt::Display for GitLoginRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} needs a login git does not have. {} ({})",
            self.url,
            self.hint(),
            self.detail
        )
    }
}

impl std::error::Error for GitLoginRequired {}

/// The plugins folder and its record.
#[derive(Debug, Clone)]
pub struct PluginStore {
    dir: PathBuf,
    /// Let git ask for a login on the terminal (the command line).
    interactive: bool,
}

impl PluginStore {
    /// The store in the configuration directory.
    pub fn new() -> Self {
        Self::at(crate::config_dir::config_dir().join("plugins"))
    }

    /// A store in `dir` (tests, other embedders).
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            interactive: false,
        }
    }

    /// Let git prompt for a login on the terminal when its credential
    /// helpers have none (for the command line; the app has no terminal).
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
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

    /// Install the plugin or add the marketplace at `source`, whichever
    /// the folder or repository holds (a marketplace manifest wins);
    /// `<plugin>@<marketplace>` installs a plugin of an added marketplace.
    pub fn add(&self, source: &str) -> Result<Added> {
        if parse_marketplace_plugin(source).is_some() {
            return self.install(source).map(Added::Plugin);
        }
        match PluginSource::parse(source)? {
            PluginSource::Local { path } if MarketplaceManifest::exists_at(&path) => self
                .finish_marketplace(PluginSource::Local { path: path.clone() }, path)
                .map(Added::Marketplace),
            PluginSource::Git { url, git_ref } => {
                let staged = self.clone_staged(&url, git_ref.as_deref())?;
                let source = PluginSource::Git { url, git_ref };
                if MarketplaceManifest::exists_at(&staged) {
                    self.finish_marketplace(source, staged)
                        .map(Added::Marketplace)
                } else {
                    self.finish_git_plugin(source, staged).map(Added::Plugin)
                }
            }
            _ => self.install(source).map(Added::Plugin),
        }
    }

    /// Install the plugin at `source` (see [`PluginSource::parse`]), or
    /// `<plugin>@<marketplace>` from an added marketplace. Fails when its
    /// manifest is invalid or a plugin of its name is installed.
    pub fn install(&self, source: &str) -> Result<PluginListing> {
        if let Some((plugin, marketplace)) = parse_marketplace_plugin(source) {
            return self.install_from_marketplace(plugin, marketplace);
        }
        match PluginSource::parse(source)? {
            PluginSource::Git { url, git_ref } => {
                let staged = self.clone_staged(&url, git_ref.as_deref())?;
                self.finish_git_plugin(PluginSource::Git { url, git_ref }, staged)
            }
            source => {
                let PluginSource::Local { path } = &source else {
                    unreachable!("parse gives git or local sources")
                };
                let manifest = PluginManifest::load(path)?;
                self.record_plugin(source.clone(), path.clone(), None, manifest)
            }
        }
    }

    /// Install `plugin` from the folder marketplace `marketplace` lists.
    fn install_from_marketplace(&self, plugin: &str, marketplace: &str) -> Result<PluginListing> {
        let added = self
            .marketplace_record()
            .marketplaces
            .remove(marketplace)
            .ok_or_else(|| anyhow!("no marketplace named '{marketplace}' is added"))?;
        let path = plugin_folder(&added, plugin, marketplace)?;
        let manifest = PluginManifest::load(&path)?;
        if manifest.name != plugin {
            bail!(
                "marketplace '{marketplace}' lists '{plugin}', but its folder holds plugin '{}'",
                manifest.name
            );
        }
        let source = PluginSource::Marketplace {
            marketplace: marketplace.to_string(),
        };
        self.record_plugin(source, path, added.commit, manifest)
    }

    /// Install the plugin cloned to `staged`.
    fn finish_git_plugin(&self, source: PluginSource, staged: PathBuf) -> Result<PluginListing> {
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
        self.record_plugin(source, target, commit, manifest)
    }

    /// Add the installed plugin to the record.
    fn record_plugin(
        &self,
        source: PluginSource,
        path: PathBuf,
        commit: Option<String>,
        manifest: PluginManifest,
    ) -> Result<PluginListing> {
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
    /// manifest checks out and still carries the same name. A plugin of a
    /// marketplace updates the marketplace, and so its other plugins.
    pub fn update(&self, name: &str) -> Result<PluginListing> {
        let mut record = self.record();
        let installed = record
            .plugins
            .get_mut(name)
            .ok_or_else(|| anyhow!("no plugin named '{name}' is installed"))?;
        let manifest = match installed.source.clone() {
            PluginSource::Marketplace { marketplace } => {
                self.update_marketplace(&marketplace)?;
                return self
                    .list()
                    .into_iter()
                    .find(|listing| listing.name == name)
                    .ok_or_else(|| anyhow!("plugin '{name}' is gone"));
            }
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

    /// Uninstall plugin `name`; its clone is deleted, a linked folder or a
    /// marketplace's folder kept.
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

    pub fn marketplaces_path(&self) -> PathBuf {
        self.dir.join(MARKETPLACES_FILE)
    }

    /// The marketplace record; empty when there is none or it cannot be
    /// read.
    pub fn marketplace_record(&self) -> MarketplaceRecord {
        let path = self.marketplaces_path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return MarketplaceRecord::default();
        };
        serde_json::from_str(&content).unwrap_or_else(|e| {
            tracing::warn!("Failed to parse {}: {e}", path.display());
            MarketplaceRecord::default()
        })
    }

    fn save_marketplaces(&self, record: &MarketplaceRecord) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        atomic_write_json(&self.marketplaces_path(), record)?;
        super::catalog::invalidate();
        Ok(())
    }

    /// Every added marketplace with its manifest.
    pub fn list_marketplaces(&self) -> Vec<MarketplaceListing> {
        self.marketplace_record()
            .marketplaces
            .into_iter()
            .map(|(name, installed)| {
                let manifest =
                    MarketplaceManifest::load(&installed.path).map_err(|e| format!("{e:#}"));
                MarketplaceListing {
                    name,
                    installed,
                    manifest,
                }
            })
            .collect()
    }

    /// Add the marketplace at `source` (a git repository or a folder).
    pub fn add_marketplace(&self, source: &str) -> Result<MarketplaceListing> {
        match PluginSource::parse(source)? {
            PluginSource::Git { url, git_ref } => {
                let staged = self.clone_staged(&url, git_ref.as_deref())?;
                self.finish_marketplace(PluginSource::Git { url, git_ref }, staged)
            }
            source => {
                let PluginSource::Local { path } = &source else {
                    unreachable!("parse gives git or local sources")
                };
                let path = path.clone();
                self.finish_marketplace(source, path)
            }
        }
    }

    /// Record the marketplace at `root`: a staged clone (moved into place)
    /// or a linked folder.
    fn finish_marketplace(
        &self,
        source: PluginSource,
        root: PathBuf,
    ) -> Result<MarketplaceListing> {
        let cloned = matches!(source, PluginSource::Git { .. });
        let discard = |e: anyhow::Error| {
            if cloned {
                let _ = std::fs::remove_dir_all(&root);
            }
            e
        };
        let manifest = MarketplaceManifest::load(&root).map_err(discard)?;
        let mut record = self.marketplace_record();
        if record.marketplaces.contains_key(&manifest.name) {
            return Err(discard(anyhow!(
                "a marketplace named '{}' is added already",
                manifest.name
            )));
        }
        let (path, commit) = if cloned {
            let commit = head_commit(&root);
            let target = self.dir.join(MARKETPLACES_DIR).join(&manifest.name);
            std::fs::create_dir_all(self.dir.join(MARKETPLACES_DIR))?;
            replace_dir(&root, &target)?;
            (target, commit)
        } else {
            (root, None)
        };
        let installed = InstalledMarketplace {
            source,
            path,
            commit,
        };
        record
            .marketplaces
            .insert(manifest.name.clone(), installed.clone());
        self.save_marketplaces(&record)?;
        tracing::info!("Added marketplace '{}'", manifest.name);
        Ok(MarketplaceListing {
            name: manifest.name.clone(),
            installed,
            manifest: Ok(manifest),
        })
    }

    /// Fetch marketplace `name` again (a linked folder is only checked) and
    /// point its installed plugins at their folders in the new commit. The
    /// clone is swapped only once the new manifest checks out and still
    /// carries the same name.
    pub fn update_marketplace(&self, name: &str) -> Result<MarketplaceListing> {
        let mut record = self.marketplace_record();
        let added = record
            .marketplaces
            .get_mut(name)
            .ok_or_else(|| anyhow!("no marketplace named '{name}' is added"))?;
        let manifest = match added.source.clone() {
            PluginSource::Git { url, git_ref } => {
                let staged = self.clone_staged(&url, git_ref.as_deref())?;
                let manifest = MarketplaceManifest::load(&staged).and_then(|manifest| {
                    if manifest.name == name {
                        Ok(manifest)
                    } else {
                        Err(anyhow!(
                            "the repository now holds marketplace '{}', not '{name}'",
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
                added.commit = head_commit(&staged);
                replace_dir(&staged, &added.path)?;
                manifest
            }
            _ => MarketplaceManifest::load(&added.path)?,
        };
        let added = added.clone();
        self.save_marketplaces(&record)?;

        let mut plugins = self.record();
        for (plugin, installed) in plugins.plugins.iter_mut() {
            if installed.source
                == (PluginSource::Marketplace {
                    marketplace: name.to_string(),
                })
            {
                installed.commit = added.commit.clone();
                // A plugin the marketplace no longer lists keeps its old
                // folder, which then fails to load and says so.
                if let Ok(path) = plugin_folder(&added, plugin, name) {
                    installed.path = path;
                }
            }
        }
        self.save(&plugins)?;
        Ok(MarketplaceListing {
            name: name.to_string(),
            installed: added,
            manifest: Ok(manifest),
        })
    }

    /// Remove marketplace `name` and the plugins installed from it; its
    /// clone is deleted, a linked folder kept.
    pub fn remove_marketplace(&self, name: &str) -> Result<()> {
        let mut record = self.marketplace_record();
        let added = record
            .marketplaces
            .remove(name)
            .ok_or_else(|| anyhow!("no marketplace named '{name}' is added"))?;
        let mut plugins = self.record();
        plugins.plugins.retain(|_, installed| {
            installed.source
                != PluginSource::Marketplace {
                    marketplace: name.to_string(),
                }
        });
        self.save(&plugins)?;
        if matches!(added.source, PluginSource::Git { .. }) && added.path.starts_with(&self.dir) {
            std::fs::remove_dir_all(&added.path)
                .with_context(|| format!("removing {}", added.path.display()))?;
        }
        self.save_marketplaces(&record)
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
            Some(git_ref) if looks_like_commit(git_ref) => self
                .run_git(&["clone", "--no-checkout", "--", url], Some(&target))
                .and_then(|()| run_git_in(&target, &["checkout", "--detach", git_ref])),
            Some(git_ref) => self.run_git(
                &["clone", "--depth", "1", "--branch", git_ref, "--", url],
                Some(&target),
            ),
            None => self.run_git(&["clone", "--depth", "1", "--", url], Some(&target)),
        };
        if let Err(e) = result {
            let _ = std::fs::remove_dir_all(&target);
            if let Some(detail) = login_failure(&format!("{e:#}")) {
                return Err(anyhow::Error::new(GitLoginRequired {
                    url: url.to_string(),
                    detail,
                }));
            }
            return Err(e.context(format!("cloning {url}")));
        }
        Ok(target)
    }

    /// Run `git args… [target]`; git may prompt for a login only when the
    /// store is [`interactive`](Self::interactive).
    fn run_git(&self, args: &[&str], target: Option<&Path>) -> Result<()> {
        if !self.interactive {
            return run_git(args, target);
        }
        let mut command = Command::new("git");
        command.args(args);
        if let Some(target) = target {
            command.arg(target);
        }
        // The terminal stays the user's: prompts and progress show there,
        // so only the exit status is known.
        let status = command
            .status()
            .context("could not run git (is it installed?)")?;
        if !status.success() {
            bail!("git {} failed ({status})", args.first().unwrap_or(&""));
        }
        Ok(())
    }
}

impl Default for PluginStore {
    fn default() -> Self {
        Self::new()
    }
}

/// The folder of `plugin` in the added marketplace `added` (named
/// `marketplace`), from its manifest.
fn plugin_folder(added: &InstalledMarketplace, plugin: &str, marketplace: &str) -> Result<PathBuf> {
    let manifest = MarketplaceManifest::load(&added.path)?;
    let entry = manifest
        .entry(plugin)
        .ok_or_else(|| anyhow!("marketplace '{marketplace}' has no plugin '{plugin}'"))?;
    entry.path_in(&added.path)
}

/// What git said when it failed for want of a login, or `None` for other
/// failures. Hosts hide private repositories behind "not found".
fn login_failure(stderr: &str) -> Option<String> {
    const SIGNS: &[&str] = &[
        "terminal prompts disabled",
        "could not read username",
        "could not read password",
        "authentication failed",
        "authentication required",
        "invalid username or password",
        "permission denied (publickey",
        "the requested url returned error: 401",
        "the requested url returned error: 403",
        "repository not found",
        "access denied",
    ];
    let lower = stderr.to_lowercase();
    SIGNS.iter().any(|sign| lower.contains(sign)).then(|| {
        stderr
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    })
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
        assert_eq!(parse_marketplace_plugin("one@acme"), Some(("one", "acme")));
        assert_eq!(parse_marketplace_plugin("git@github.com:acme/x.git"), None);
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

    /// A git repository with marketplace `acme` listing plugins `one` and
    /// `two` in folders, as a `file://` URL.
    fn marketplace_repo(dir: &Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
        write_marketplace(dir, &[("one", "./plugins/one"), ("two", "plugins/two")]);
        write_plugin(&dir.join("plugins/one"), "one", "first");
        write_plugin(&dir.join("plugins/two"), "two", "first");
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "first"]);
        format!("file://{}", dir.display())
    }

    fn write_marketplace(dir: &Path, plugins: &[(&str, &str)]) {
        let entries: Vec<String> = plugins
            .iter()
            .map(|(name, source)| format!(r#"{{ "name": "{name}", "source": "{source}" }}"#))
            .collect();
        let path = dir.join(crate::plugin_host::MARKETPLACE_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!(
                r#"{{ "name": "acme", "plugins": [{}] }}"#,
                entries.join(",")
            ),
        )
        .unwrap();
    }

    #[test]
    fn plugins_install_from_an_added_marketplace() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let url = marketplace_repo(&repo);
        let store = PluginStore::at(temp.path().join("plugins"));

        let Added::Marketplace(added) = store.add(&url).unwrap() else {
            panic!("a marketplace expected");
        };
        assert_eq!(added.name, "acme");
        let clone = store.dir().join(MARKETPLACES_DIR).join("acme");
        assert_eq!(added.installed.path, clone);
        assert_eq!(added.manifest.unwrap().plugins.len(), 2);
        assert!(store.add(&url).is_err(), "a second add of the name");
        assert!(store.install("three@acme").is_err(), "not listed");
        assert!(store.install("one@other").is_err(), "no such marketplace");

        let one = store.install("one@acme").unwrap();
        assert_eq!(one.installed.path, clone.join("plugins").join("one"));
        assert_eq!(one.installed.commit, added.installed.commit);
        assert_eq!(one.installed.source.display(), "marketplace acme");
        let Added::Plugin(two) = store.add("two@acme").unwrap() else {
            panic!("a plugin expected");
        };
        assert_eq!(two.name, "two");

        // Updating the marketplace updates its plugins, which may move.
        std::fs::rename(repo.join("plugins/one"), repo.join("one")).unwrap();
        write_marketplace(&repo, &[("one", "one"), ("two", "plugins/two")]);
        write_plugin(&repo.join("one"), "one", "second");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "second"]);
        let updated = store.update("one").unwrap();
        assert_eq!(updated.installed.path, clone.join("one"));
        assert_eq!(
            updated.manifest.unwrap().description.as_deref(),
            Some("second")
        );
        assert_ne!(updated.installed.commit, one.installed.commit);
        assert_eq!(
            store.record().plugins["two"].commit,
            updated.installed.commit
        );

        // Removing a plugin keeps the marketplace's clone; removing the
        // marketplace removes the clone and the plugins installed from it.
        store.remove("two").unwrap();
        assert!(clone.join("plugins/two").exists());
        store.remove_marketplace("acme").unwrap();
        assert!(store.record().plugins.is_empty());
        assert!(store.list_marketplaces().is_empty());
        assert!(!clone.exists());
    }

    #[test]
    fn a_folder_with_a_marketplace_is_linked_as_one() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("market");
        write_marketplace(&folder, &[("one", "plugins/one")]);
        write_plugin(&folder.join("plugins/one"), "one", "local");
        let store = PluginStore::at(temp.path().join("plugins"));
        let Added::Marketplace(added) = store.add(folder.to_str().unwrap()).unwrap() else {
            panic!("a marketplace expected");
        };
        assert_eq!(
            added.installed.path,
            std::fs::canonicalize(&folder).unwrap()
        );
        store.install("one@acme").unwrap();
        store.remove_marketplace("acme").unwrap();
        assert!(folder.join("plugins/one").join(MANIFEST_PATH).exists());
    }

    #[test]
    fn a_plugin_must_carry_the_name_its_marketplace_gives_it() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("market");
        write_marketplace(&folder, &[("one", "plugins/one")]);
        write_plugin(&folder.join("plugins/one"), "other", "local");
        let store = PluginStore::at(temp.path().join("plugins"));
        store.add_marketplace(folder.to_str().unwrap()).unwrap();
        assert!(store.install("one@acme").is_err());
        assert!(store.record().plugins.is_empty());
    }

    #[test]
    fn a_repository_that_needs_a_login_says_so() {
        let temp = tempfile::tempdir().unwrap();
        let store = PluginStore::at(temp.path().join("plugins"));
        // A server that refuses every request with 401, as a host does for
        // a private repository without a login.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                use std::io::{BufRead, BufReader, Write};
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let _ = (&stream).write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"git\"\r\n\
                      Content-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        let url = format!("http://127.0.0.1:{port}/acme/private.git");
        // No credential helper of the user's may answer for the test host.
        let error = store
            .add(&format!("{url}#main"))
            .expect_err("no login available");
        let login = error
            .downcast_ref::<GitLoginRequired>()
            .unwrap_or_else(|| panic!("a login error expected, got {error:#}"));
        assert_eq!(login.url, url);
        assert!(!login.is_ssh());
        assert!(login.to_string().contains("credential helper"));
    }

    #[test]
    fn login_failures_are_told_from_other_failures() {
        assert!(
            login_failure(
                "fatal: could not read Username for 'https://github.com': terminal prompts disabled"
            )
            .is_some()
        );
        assert!(
            login_failure(
                "ERROR: Repository not found.\nfatal: Could not read from remote repository."
            )
            .is_some()
        );
        assert!(login_failure("git@github.com: Permission denied (publickey).").is_some());
        assert!(login_failure("fatal: Remote branch v9 not found in upstream origin").is_none());
        assert!(
            GitLoginRequired {
                url: "git@github.com:acme/x.git".into(),
                detail: String::new()
            }
            .is_ssh()
        );
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
