//! The marketplace manifest, `.code-assistant-plugin/marketplace.json` at a
//! repository's root: a repository holding several plugins, each in a
//! folder of its own.
//!
//! ```jsonc
//! {
//!   "name": "acme",
//!   "description": "ACME's plugins",
//!   "owner": { "name": "ACME Platform Team", "email": "platform@acme.example" },
//!   "plugins": [
//!     { "name": "diagrams", "source": "./plugins/diagrams", "description": "draw.io renderer" },
//!     { "name": "gateway", "source": "./plugins/gateway" }
//!   ]
//! }
//! ```
//!
//! Each `source` is a folder inside the repository holding a complete plugin
//! (its own `plugin.json`, whose name must be the entry's). Entries never
//! point to other repositories: one clone, one commit and one review per
//! marketplace. Parsing is strict, like the plugin manifest's.

use super::manifest::is_valid_name;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

/// Where the marketplace manifest lives, relative to the repository root.
pub const MARKETPLACE_PATH: &str = ".code-assistant-plugin/marketplace.json";

/// A marketplace's manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketplaceManifest {
    /// `[a-z0-9-]`, at most 64 characters; plugins install as
    /// `<plugin>@<name>`.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<MarketplaceOwner>,
    pub plugins: Vec<MarketplaceEntry>,
}

/// Who maintains a marketplace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketplaceOwner {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A plugin a marketplace offers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarketplaceEntry {
    /// The plugin's name, as its own manifest gives it.
    pub name: String,
    /// The plugin's folder, relative to the repository root (`./plugins/x`;
    /// `./` for a plugin at the root).
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl MarketplaceManifest {
    /// Read and validate the manifest of the marketplace at `root`.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(MARKETPLACE_PATH);
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("No marketplace manifest at {}", path.display()))?;
        Self::parse(&content).with_context(|| format!("Invalid marketplace {}", path.display()))
    }

    /// Whether the folder at `root` is a marketplace.
    pub fn exists_at(root: &Path) -> bool {
        root.join(MARKETPLACE_PATH).is_file()
    }

    pub fn parse(content: &str) -> Result<Self> {
        let manifest: Self = serde_json::from_str(content)?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        if !is_valid_name(&self.name) {
            bail!(
                "marketplace name '{}' must be 1–64 characters of a-z, 0-9 and '-'",
                self.name
            );
        }
        let mut names = HashSet::new();
        for entry in &self.plugins {
            if !is_valid_name(&entry.name) {
                bail!(
                    "plugin name '{}' must be 1–64 characters of a-z, 0-9 and '-'",
                    entry.name
                );
            }
            if !names.insert(entry.name.as_str()) {
                bail!("plugin '{}' is listed twice", entry.name);
            }
            entry
                .folder()
                .with_context(|| format!("plugin '{}'", entry.name))?;
        }
        Ok(())
    }

    /// The entry for plugin `name`.
    pub fn entry(&self, name: &str) -> Option<&MarketplaceEntry> {
        self.plugins.iter().find(|entry| entry.name == name)
    }
}

impl MarketplaceEntry {
    /// The plugin's folder relative to the repository root; an error when
    /// `source` is absolute or leaves the repository.
    pub fn folder(&self) -> Result<PathBuf> {
        let source = self.source.trim();
        if source.is_empty() {
            bail!("`source` is empty");
        }
        if source.contains("://") || source.starts_with("git@") {
            bail!("`source` must be a folder in the marketplace repository, not '{source}'");
        }
        let mut folder = PathBuf::new();
        for component in Path::new(source).components() {
            match component {
                Component::CurDir => {}
                Component::Normal(part) => folder.push(part),
                _ => bail!("`source` must be a folder inside the repository, not '{source}'"),
            }
        }
        Ok(folder)
    }

    /// The plugin's folder in the marketplace at `root`.
    pub fn path_in(&self, root: &Path) -> Result<PathBuf> {
        Ok(root.join(self.folder()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marketplace_lists_plugins_in_its_folders() {
        let manifest = MarketplaceManifest::parse(
            r#"{
                "name": "acme",
                "owner": { "name": "ACME" },
                "plugins": [
                    { "name": "diagrams", "source": "./plugins/diagrams", "description": "d" },
                    { "name": "root", "source": "./" }
                ]
            }"#,
        )
        .unwrap();
        let root = Path::new("/repo");
        let diagrams = manifest.entry("diagrams").unwrap();
        assert_eq!(
            diagrams.path_in(root).unwrap(),
            root.join("plugins").join("diagrams")
        );
        assert_eq!(manifest.entry("root").unwrap().path_in(root).unwrap(), root);
    }

    #[test]
    fn sources_stay_inside_the_repository() {
        let parse = |source: &str| {
            MarketplaceManifest::parse(&format!(
                r#"{{ "name": "m", "plugins": [{{ "name": "p", "source": "{source}" }}] }}"#
            ))
        };
        assert!(parse("plugins/p").is_ok());
        assert!(parse("../p").is_err());
        assert!(parse("./a/../../p").is_err());
        assert!(parse("/etc").is_err());
        assert!(parse("https://github.com/acme/p").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn names_are_checked() {
        let bad_name = r#"{ "name": "Acme", "plugins": [] }"#;
        assert!(MarketplaceManifest::parse(bad_name).is_err());
        let twice = r#"{ "name": "m", "plugins": [
            { "name": "p", "source": "a" }, { "name": "p", "source": "b" }] }"#;
        assert!(MarketplaceManifest::parse(twice).is_err());
        let unknown = r#"{ "name": "m", "plugins": [], "extra": 1 }"#;
        assert!(MarketplaceManifest::parse(unknown).is_err());
    }
}
