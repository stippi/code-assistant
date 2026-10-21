//! MCP resources.
//!
//! MCP leaves it to the host how resources reach the model. Registration
//! leaves every server declaring the `resources` capability, with what it
//! listed, in the registry as the [`McpResourceServers`] extension; the
//! embedder offers them through its own tools (code-assistant: an
//! `:mcp:<server>` scope for its file tools).

use crate::client::McpServerConnection;
use crate::output::describe_resource;
use anyhow::Result;
use rmcp::model::{Resource, ResourceTemplate, Role};
use std::collections::BTreeMap;
use std::sync::Arc;

/// How many resources and templates [`ResourceCatalog::render_summary`]
/// names.
const SUMMARY_RESOURCES: usize = 25;
const SUMMARY_TEMPLATES: usize = 10;
/// Longest summary line, in characters; longer ones (long descriptions) are
/// cut.
const SUMMARY_LINE_CHARS: usize = 200;

/// The servers offering resources, by server name — left with the registry
/// that registered their tools.
#[derive(Clone, Default)]
pub struct McpResourceServers {
    servers: BTreeMap<String, ResourceServer>,
}

/// A server offering resources: its connection and what it listed when its
/// tools were registered.
#[derive(Clone)]
pub struct ResourceServer {
    pub connection: Arc<McpServerConnection>,
    pub catalog: ResourceCatalog,
}

impl McpResourceServers {
    pub fn insert(&mut self, server: ResourceServer) {
        self.servers
            .insert(server.connection.name().to_string(), server);
    }

    pub fn get(&self, name: &str) -> Option<&ResourceServer> {
        self.servers.get(name)
    }

    /// The servers in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ResourceServer)> {
        self.servers
            .iter()
            .map(|(name, server)| (name.as_str(), server))
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// These servers without the named ones (e.g. switched off).
    pub fn without(&self, names: &[String]) -> Self {
        Self {
            servers: self
                .servers
                .iter()
                .filter(|(name, _)| !names.contains(name))
                .map(|(name, server)| (name.clone(), server.clone()))
                .collect(),
        }
    }
}

/// What a server lists: resources and resource templates.
#[derive(Debug, Default, Clone)]
pub struct ResourceCatalog {
    pub resources: Vec<Resource>,
    pub templates: Vec<ResourceTemplate>,
}

impl ResourceCatalog {
    /// List a server's resources and templates. Templates are optional for
    /// a server: failing to list them leaves them out.
    pub async fn discover(connection: &McpServerConnection) -> Result<Self> {
        let resources = connection.list_resources().await?;
        let templates = connection
            .list_resource_templates()
            .await
            .inspect_err(|error| {
                tracing::debug!(
                    server = connection.name(),
                    "no resource templates: {error:#}"
                )
            })
            .unwrap_or_default();
        Ok(Self {
            resources,
            templates,
        })
    }

    /// Every resource whose URI starts with `prefix` (all for an empty one),
    /// and every template, one per line.
    pub fn render_listing(&self, prefix: &str) -> String {
        let resources: Vec<&Resource> = self
            .resources
            .iter()
            .filter(|resource| resource.uri.starts_with(prefix))
            .collect();
        let mut text = String::new();
        if !resources.is_empty() {
            text.push_str("Resources:\n");
            for resource in resources {
                text.push_str(&format!("- {}\n", describe_resource(resource)));
            }
        }
        if !self.templates.is_empty() {
            text.push_str(
                "Resource templates (fill in the {placeholders} to get a readable URI):\n",
            );
            for template in &self.templates {
                text.push_str(&format!("- {}\n", describe_template(template)));
            }
        }
        if text.is_empty() {
            return if prefix.is_empty() {
                "The server lists no resources.".to_string()
            } else {
                format!("The server lists no resources starting with '{prefix}'.")
            };
        }
        text.trim_end().to_string()
    }

    /// A capped summary, one line per resource or template. Resources meant
    /// only for the user (`audience: ["user"]`) are left out; the others are
    /// ordered by their `priority` annotation. `more_hint` follows the count
    /// of what did not fit. `None` when there is nothing to name.
    pub fn render_summary(&self, more_hint: &str) -> Option<String> {
        let mut resources: Vec<&Resource> = self
            .resources
            .iter()
            .filter(|resource| !for_user_only(resource))
            .collect();
        resources.sort_by(|a, b| priority(b).total_cmp(&priority(a)));
        let mut lines: Vec<String> = resources
            .iter()
            .take(SUMMARY_RESOURCES)
            .map(|resource| truncate(&describe_resource(resource)))
            .collect();
        if resources.len() > SUMMARY_RESOURCES {
            lines.push(format!(
                "… and {} more resources: {more_hint}",
                resources.len() - SUMMARY_RESOURCES
            ));
        }
        lines.extend(
            self.templates
                .iter()
                .take(SUMMARY_TEMPLATES)
                .map(|template| {
                    truncate(&format!(
                        "{} (template: fill in the {{placeholders}})",
                        describe_template(template)
                    ))
                }),
        );
        if self.templates.len() > SUMMARY_TEMPLATES {
            lines.push(format!(
                "… and {} more templates: {more_hint}",
                self.templates.len() - SUMMARY_TEMPLATES
            ));
        }
        (!lines.is_empty()).then(|| {
            lines
                .iter()
                .map(|line| format!("- {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
    }
}

fn for_user_only(resource: &Resource) -> bool {
    resource
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.audience.as_ref())
        .is_some_and(|audience| !audience.is_empty() && !audience.contains(&Role::Assistant))
}

fn priority(resource: &Resource) -> f32 {
    resource
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.priority)
        .unwrap_or(0.5)
}

fn describe_template(template: &ResourceTemplate) -> String {
    let mut line = format!(
        "{} — {}",
        template.uri_template,
        template.title.as_deref().unwrap_or(&template.name)
    );
    if let Some(mime_type) = &template.mime_type {
        line.push_str(&format!(" ({mime_type})"));
    }
    if let Some(description) = template.description.as_deref().filter(|d| !d.is_empty()) {
        line.push_str(": ");
        line.push_str(description);
    }
    line
}

fn truncate(line: &str) -> String {
    if line.chars().count() <= SUMMARY_LINE_CHARS {
        return line.to_string();
    }
    format!(
        "{}…",
        line.chars().take(SUMMARY_LINE_CHARS).collect::<String>()
    )
}
