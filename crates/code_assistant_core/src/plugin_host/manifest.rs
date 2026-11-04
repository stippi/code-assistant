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
//!   "fileRenderers": [{
//!     "label": "Graphviz", "files": ["*.dot", "*.gv"],
//!     "kind": "command", "command": "dot -Tsvg", "output": "svg"
//!   }, {
//!     "label": "draw.io", "files": ["*.drawio"], "kind": "web",
//!     "url": "http://localhost:8080/?embed=1&proto=json&dark={dark}",
//!     "start": { "command": "scripts/start-drawio.sh" }
//!   }]
//! }
//! ```
//!
//! File renderers have the shape the Files view's renderers take (see
//! `docs/file-renderers.md`), plus a `start` command for a server they need.
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
    /// Ways to show files in the Files view: a command turning a file into
    /// an image, or a web editor editing it in place.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_renderers: Vec<PluginFileRenderer>,
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

/// A file renderer for the Files view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginFileRenderer {
    /// The name in the Files view's dropdown; unique within the plugin.
    pub label: String,
    /// Patterns for the file names it takes (`*.dot`): `*` and `?` are
    /// wildcards, case is ignored.
    pub files: Vec<String>,
    pub kind: RendererKind,
    /// `command`: a shell command line, run in the project root; `{file}`
    /// is the file, `{output}` where to write the result (else standard
    /// output), the text is on standard input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// `command`: what it produces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<RendererOutput>,
    /// `web`: the editor's address, talking the embed protocol; `{dark}`
    /// is `1` in a dark theme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// `web`: scripts injected into the editor, relative to the plugin root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scripts: Vec<String>,
    /// Starts a server the renderer needs, when nothing answers at its
    /// address (`start.url`, for a web editor its `url` by default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<ServerStart>,
}

/// What a file renderer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RendererKind {
    /// A command turning the file into an SVG or an image.
    Command,
    /// A web app editing the file in place.
    Web,
}

/// What a command renderer produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RendererOutput {
    #[default]
    Svg,
    #[serde(alias = "png")]
    Image,
}

/// A command that starts the server a renderer needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerStart {
    /// The program; a relative path is taken from the plugin root.
    pub command: String,
    /// Where the server answers once it runs. A web editor's own `url` when
    /// left out; needed for a command renderer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    /// How long to wait for the server to answer.
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
        let mut labels = HashSet::new();
        for renderer in &self.file_renderers {
            if renderer.label.trim().is_empty() || !labels.insert(renderer.label.as_str()) {
                bail!("file renderer labels must be unique and non-empty");
            }
            renderer
                .validate()
                .with_context(|| format!("file renderer '{}'", renderer.label))?;
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
            count(self.file_renderers.len(), "file renderer", "file renderers"),
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

impl PluginFileRenderer {
    fn validate(&self) -> Result<()> {
        if self.files.iter().all(|pattern| pattern.trim().is_empty()) {
            bail!("takes no files");
        }
        match self.kind {
            RendererKind::Command => {
                if self.command.as_deref().is_none_or(|c| c.trim().is_empty()) {
                    bail!("a command renderer needs a `command`");
                }
                if self.url.is_some() || !self.scripts.is_empty() {
                    bail!("`url` and `scripts` are for web editors");
                }
                if self.start.as_ref().is_some_and(|s| s.url.is_none()) {
                    bail!("`start` of a command renderer needs a `url` to wait for");
                }
            }
            RendererKind::Web => {
                if self.command.is_some() || self.output.is_some() {
                    bail!("`command` and `output` are for command renderers");
                }
                if !self.url.as_deref().is_some_and(is_http) {
                    bail!("a web editor needs an http(s) `url`");
                }
            }
        }
        if let Some(url) = self.start.as_ref().and_then(|s| s.url.as_deref())
            && !is_http(url)
        {
            bail!("`start.url` must be http(s)");
        }
        Ok(())
    }

    /// The address a started server answers at.
    pub fn server_url(&self) -> Option<&str> {
        let start = self.start.as_ref()?;
        start.url.as_deref().or(self.url.as_deref())
    }
}

fn is_http(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
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
        "fileRenderers": [{
            "label": "Graphviz", "files": ["*.dot"], "kind": "command",
            "command": "dot -Tsvg"
        }, {
            "label": "draw.io", "files": ["*.drawio", "*.drawio.svg"], "kind": "web",
            "url": "http://localhost:8080/?embed=1&proto=json&dark={dark}",
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
        let [graphviz, drawio] = &manifest.file_renderers[..] else {
            panic!("two renderers");
        };
        assert_eq!(graphviz.kind, RendererKind::Command);
        assert_eq!(graphviz.server_url(), None);
        assert_eq!(drawio.start.as_ref().unwrap().timeout_seconds, 120);
        assert_eq!(drawio.server_url(), drawio.url.as_deref());
        assert_eq!(
            manifest.contributions_summary(),
            "1 provider template, 2 MCP servers, 2 file renderers"
        );
    }

    #[test]
    fn the_example_plugin_is_valid() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugin");
        let manifest = PluginManifest::load(&root).unwrap();
        assert_eq!(manifest.name, "example");
        assert_eq!(
            manifest.contributions_summary(),
            "2 provider templates, 1 MCP server, 2 file renderers"
        );
        let drawio = manifest
            .file_renderers
            .iter()
            .find(|r| r.kind == RendererKind::Web)
            .unwrap();
        let start = drawio.start.as_ref().unwrap();
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
    }

    #[test]
    fn renderers_carry_what_their_kind_needs() {
        let parse = |renderer: &str| {
            PluginManifest::parse(&format!(
                r#"{{ "name": "a", "fileRenderers": [{renderer}] }}"#
            ))
        };
        assert!(
            parse(r#"{ "label": "W", "files": ["*.x"], "kind": "web", "url": "file:///x" }"#)
                .is_err()
        );
        assert!(parse(r#"{ "label": "W", "files": ["*.x"], "kind": "web", "url": "http://h/", "command": "x" }"#).is_err());
        assert!(parse(r#"{ "label": "C", "files": ["*.x"], "kind": "command" }"#).is_err());
        assert!(
            parse(r#"{ "label": "C", "files": [], "kind": "command", "command": "x" }"#).is_err()
        );
        assert!(
            parse(
                r#"{ "label": "C", "files": ["*.x"], "kind": "command", "command": "x",
                           "start": { "command": "s" } }"#
            )
            .is_err(),
            "nothing to wait for"
        );
        assert!(parse(r#"{ "label": "C", "files": ["*.x"], "kind": "command", "command": "x",
                           "output": "png", "start": { "command": "s", "url": "http://localhost:1/" } }"#).is_ok());
        assert!(parse(r#"{ "label": "C", "files": ["*.x"], "kind": "pdf" }"#).is_err());
        let twice = r#"{ "label": "C", "files": ["*.x"], "kind": "command", "command": "x" }"#;
        assert!(
            parse(&format!("{twice}, {twice}")).is_err(),
            "labels are unique"
        );
    }

    #[test]
    fn start_commands_resolve_against_the_plugin_root() {
        let root = Path::new("/plugins/acme");
        let start = |command: &str| ServerStart {
            command: command.into(),
            url: None,
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
