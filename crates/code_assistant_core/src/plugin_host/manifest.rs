//! The plugin manifest, `.code-assistant-plugin/plugin.json` at a plugin's
//! root: what the plugin is and what it contributes.
//!
//! ```jsonc
//! {
//!   "name": "acme",
//!   "version": "1.0.0",
//!   "description": "ACME gateway, tools and diagrams",
//!   "providerTemplates": [{
//!     "id": "gateway", "label": "ACME Gateway", "provider": "openai-responses",
//!     "config": { "base_url": "https://llm.acme.example/v1" },
//!     "models": ["gpt-5.5"]
//!   }],
//!   "mcpServers": {
//!     "acme-tools": { "command": "${PLUGIN_ROOT}/bin/acme-mcp", "args": ["--stdio"] }
//!   },
//!   "fileViewers": [{
//!     "id": "drawio", "label": "draw.io", "files": ["*.drawio", "*.drawio.svg"],
//!     "url": "http://localhost:8080/", "protocol": "drawio",
//!     "start": { "command": "scripts/start-drawio.sh" }
//!   }]
//! }
//! ```
//!
//! Parsing is strict: unknown keys are errors, so a typo does not silently
//! drop a contribution.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Where the manifest lives, relative to the plugin root.
pub const MANIFEST_PATH: &str = ".code-assistant-plugin/plugin.json";

/// A plugin's manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginManifest {
    /// `[a-z0-9-]`, at most 64 characters; unique among installed plugins.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Prefilled providers offered when adding a provider.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_templates: Vec<ProviderTemplate>,
    /// MCP servers, in the `.mcp.json` entry format. They are added while
    /// the plugin is enabled and cannot be edited in the settings.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp_servers: BTreeMap<String, Value>,
    /// Ways to show files in the Files view, as web pages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_viewers: Vec<FileViewerSpec>,
}

/// A prefilled provider entry for `providers.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderTemplate {
    /// Unique within the plugin.
    pub id: String,
    /// The provider's label, and the template's name in the picker.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The provider type (`anthropic`, `openai-responses`, `ollama`, ...).
    pub provider: String,
    /// The provider's `config` object (`base_url`, extra fields). Never put
    /// secrets here: the user fills in the API key.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub config: Map<String, Value>,
    /// Model IDs to offer in the model picker once the provider is added.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
}

/// A file viewer shown as a web page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileViewerSpec {
    /// Unique within the plugin.
    pub id: String,
    /// The name in the Files view's dropdown.
    pub label: String,
    /// File name patterns the viewer takes (`*.drawio`), case-insensitive.
    pub files: Vec<String>,
    /// The page to show. `{path}` and `{relative_path}` are replaced by the
    /// file's absolute and project-relative path (URL-encoded).
    pub url: String,
    /// How the page gets the file's text and gives edits back.
    #[serde(default)]
    pub protocol: ViewerProtocol,
    /// Starts the server behind `url` when nothing answers there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<ServerStart>,
}

/// How a viewer page and the app exchange the file's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewerProtocol {
    /// The page uses `window.codeAssistant` (`onText`, `setText`).
    #[default]
    HostApi,
    /// draw.io's embed protocol; the app adds `embed=1&proto=json`.
    Drawio,
    /// No exchange: the page shows the file on its own (from the URL).
    None,
}

/// A command that starts a viewer's server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerStart {
    /// The program; a relative path is taken from the plugin root.
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    /// How long to wait for the server to answer at `url`.
    #[serde(default = "default_start_timeout")]
    pub timeout_seconds: u64,
}

fn default_start_timeout() -> u64 {
    120
}

impl PluginManifest {
    /// Read and validate the manifest of the plugin at `root`.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(MANIFEST_PATH);
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("No plugin manifest at {}", path.display()))?;
        Self::parse(&content).with_context(|| format!("Invalid manifest {}", path.display()))
    }

    pub fn parse(content: &str) -> Result<Self> {
        let manifest: Self = serde_json::from_str(content)?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<()> {
        if !is_valid_name(&self.name) {
            bail!(
                "plugin name '{}' must be 1–64 characters of a-z, 0-9 and '-'",
                self.name
            );
        }
        let mut ids = HashSet::new();
        for template in &self.provider_templates {
            if template.id.is_empty() || !ids.insert(template.id.as_str()) {
                bail!("provider template ids must be unique and non-empty");
            }
            if template.provider.is_empty() {
                bail!("provider template '{}' names no provider", template.id);
            }
        }
        let mut ids = HashSet::new();
        for viewer in &self.file_viewers {
            if viewer.id.is_empty() || !ids.insert(viewer.id.as_str()) {
                bail!("file viewer ids must be unique and non-empty");
            }
            if viewer.files.is_empty() {
                bail!("file viewer '{}' takes no files", viewer.id);
            }
            for pattern in &viewer.files {
                glob::Pattern::new(pattern).with_context(|| {
                    format!("file viewer '{}': bad pattern '{pattern}'", viewer.id)
                })?;
            }
            if !(viewer.url.starts_with("http://") || viewer.url.starts_with("https://")) {
                bail!("file viewer '{}': url must be http(s)", viewer.id);
            }
        }
        // The MCP entries parse as `.mcp.json` would.
        self.mcp_servers_config()?;
        Ok(())
    }

    /// The MCP servers as configuration, `${PLUGIN_ROOT}` not yet replaced.
    pub fn mcp_servers_config(&self) -> Result<mcp_client::McpServersConfig> {
        if self.mcp_servers.is_empty() {
            return Ok(mcp_client::McpServersConfig::default());
        }
        let file = serde_json::json!({ "mcpServers": self.mcp_servers });
        mcp_client::parse_local_mcp_json(&file.to_string())
    }

    /// A one-line summary of the contributions ("1 provider template, …").
    pub fn contributions_summary(&self) -> String {
        let count = |n: usize, one: &str, many: &str| match n {
            0 => None,
            1 => Some(format!("1 {one}")),
            n => Some(format!("{n} {many}")),
        };
        let parts: Vec<String> = [
            count(
                self.provider_templates.len(),
                "provider template",
                "provider templates",
            ),
            count(self.mcp_servers.len(), "MCP server", "MCP servers"),
            count(self.file_viewers.len(), "file viewer", "file viewers"),
        ]
        .into_iter()
        .flatten()
        .collect();
        if parts.is_empty() {
            "No contributions".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// `[a-z0-9-]{1,64}`.
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl FileViewerSpec {
    /// Whether the viewer takes the file at `path` (by its file name).
    pub fn accepts(&self, path: &str) -> bool {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let options = glob::MatchOptions {
            case_sensitive: false,
            ..Default::default()
        };
        self.files
            .iter()
            .any(|pattern| glob::Pattern::new(pattern).is_ok_and(|p| p.matches_with(name, options)))
    }
}

impl ServerStart {
    /// The program to run: a relative path is taken from the plugin root.
    pub fn program(&self, root: &Path) -> PathBuf {
        let command = substitute_root(&self.command, root);
        let path = PathBuf::from(&command);
        if path.is_relative() && (command.contains('/') || root.join(&path).exists()) {
            root.join(path)
        } else {
            path
        }
    }
}

/// Replace `${PLUGIN_ROOT}` with the plugin's root folder.
pub fn substitute_root(value: &str, root: &Path) -> String {
    value.replace("${PLUGIN_ROOT}", &root.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{
        "name": "acme",
        "version": "1.0.0",
        "providerTemplates": [{
            "id": "gateway", "label": "ACME Gateway", "provider": "openai-responses",
            "config": { "base_url": "https://llm.acme.example/v1" },
            "models": ["gpt-5.5"]
        }],
        "mcpServers": {
            "acme-tools": { "command": "${PLUGIN_ROOT}/bin/acme-mcp", "args": ["--stdio"] },
            "acme-remote": { "type": "http", "url": "https://mcp.acme.example/mcp" }
        },
        "fileViewers": [{
            "id": "drawio", "label": "draw.io", "files": ["*.drawio", "*.drawio.svg"],
            "url": "http://localhost:8080/", "protocol": "drawio",
            "start": { "command": "scripts/start.sh" }
        }]
    }"#;

    #[test]
    fn a_full_manifest_parses() {
        let manifest = PluginManifest::parse(FULL).unwrap();
        assert_eq!(manifest.name, "acme");
        assert_eq!(manifest.provider_templates[0].provider, "openai-responses");
        let servers = manifest.mcp_servers_config().unwrap();
        assert!(servers.servers["acme-remote"].transport.is_http());
        let viewer = &manifest.file_viewers[0];
        assert_eq!(viewer.protocol, ViewerProtocol::Drawio);
        assert_eq!(viewer.start.as_ref().unwrap().timeout_seconds, 120);
        assert_eq!(
            manifest.contributions_summary(),
            "1 provider template, 2 MCP servers, 1 file viewer"
        );
    }

    #[test]
    fn the_example_plugin_is_valid() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugin");
        let manifest = PluginManifest::load(&root).unwrap();
        assert_eq!(manifest.name, "example");
        assert_eq!(
            manifest.contributions_summary(),
            "2 provider templates, 1 MCP server, 1 file viewer"
        );
        let start = manifest.file_viewers[0].start.as_ref().unwrap();
        assert!(
            start.program(&root).exists(),
            "the start script ships along"
        );
    }

    #[test]
    fn unknown_keys_and_bad_names_are_rejected() {
        assert!(PluginManifest::parse(r#"{ "name": "a", "mcpServer": {} }"#).is_err());
        assert!(PluginManifest::parse(r#"{ "name": "Not Valid" }"#).is_err());
        assert!(PluginManifest::parse(r#"{ "name": "ok-1" }"#).is_ok());
        let bad_server = r#"{ "name": "a", "mcpServers": { "x": { "type": "stdio" } } }"#;
        assert!(PluginManifest::parse(bad_server).is_err());
        let bad_url = r#"{ "name": "a", "fileViewers": [
            { "id": "v", "label": "V", "files": ["*.x"], "url": "file:///tmp" }] }"#;
        assert!(PluginManifest::parse(bad_url).is_err());
    }

    #[test]
    fn viewers_match_file_names_case_insensitively() {
        let manifest = PluginManifest::parse(FULL).unwrap();
        let viewer = &manifest.file_viewers[0];
        assert!(viewer.accepts("docs/flow.drawio"));
        assert!(viewer.accepts("docs/Flow.DRAWIO.svg"));
        assert!(!viewer.accepts("docs/logo.svg"));
        assert!(!viewer.accepts("drawio/readme.md"));
    }

    #[test]
    fn start_commands_resolve_against_the_plugin_root() {
        let root = Path::new("/plugins/acme");
        let start = |command: &str| ServerStart {
            command: command.into(),
            args: vec![],
            env: HashMap::new(),
            timeout_seconds: 1,
        };
        assert_eq!(
            start("scripts/start.sh").program(root),
            Path::new("/plugins/acme/scripts/start.sh")
        );
        assert_eq!(
            start("${PLUGIN_ROOT}/run").program(root),
            Path::new("/plugins/acme/run")
        );
        assert_eq!(start("docker").program(root), Path::new("docker"));
    }
}
