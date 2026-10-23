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
    /// List a server's resources and templates, both at once. Templates are
    /// optional for a server: failing to list them leaves them out.
    pub async fn discover(connection: &McpServerConnection) -> Result<Self> {
        let (resources, templates) = tokio::join!(
            connection.list_resources(),
            connection.list_resource_templates()
        );
        let templates = templates
            .inspect_err(|error| {
                tracing::debug!(
                    server = connection.name(),
                    "no resource templates: {error:#}"
                )
            })
            .unwrap_or_default();
        Ok(Self {
            resources: resources?,
            templates,
        })
    }

    /// Whether the server listed a resource with this URI.
    pub fn lists(&self, uri: &str) -> bool {
        self.resources.iter().any(|resource| resource.uri == uri)
    }

    /// Whether `uri` fits one of the server's templates: their literal parts
    /// in order, anything in place of the `{expressions}`.
    pub fn matches_template(&self, uri: &str) -> bool {
        self.templates
            .iter()
            .any(|template| template_matches(&template.uri_template, uri))
    }

    /// Every resource whose URI starts with `prefix` (all for an empty one),
    /// and every template that can produce such a URI, one per line.
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
        let templates: Vec<&ResourceTemplate> = self
            .templates
            .iter()
            .filter(|template| {
                let fixed = literal_prefix(&template.uri_template);
                fixed.starts_with(prefix) || prefix.starts_with(fixed)
            })
            .collect();
        if !templates.is_empty() {
            text.push_str(
                "Resource templates (fill in the {placeholders} to get a readable URI):\n",
            );
            for template in templates {
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

/// The part of a URI template before its first expression.
fn literal_prefix(uri_template: &str) -> &str {
    uri_template
        .split_once('{')
        .map_or(uri_template, |(prefix, _)| prefix)
}

/// Whether `uri` fits `uri_template` (RFC 6570): its literal parts in order,
/// anything in place of an expression.
fn template_matches(uri_template: &str, uri: &str) -> bool {
    let mut parts = uri_template.split('{');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = uri.strip_prefix(first) else {
        return false;
    };
    let mut literals = Vec::new();
    for part in parts {
        let Some((_expression, literal)) = part.split_once('}') else {
            return false;
        };
        literals.push(literal);
    }
    let Some(last) = literals.pop() else {
        return rest.is_empty();
    };
    for literal in literals {
        match rest.find(literal) {
            Some(at) => rest = &rest[at + literal.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(resources: &[&str], templates: &[&str]) -> ResourceCatalog {
        ResourceCatalog {
            resources: resources
                .iter()
                .map(|uri| Resource::new(*uri, *uri))
                .collect(),
            templates: templates
                .iter()
                .map(|uri_template| {
                    serde_json::from_value(serde_json::json!({
                        "uriTemplate": uri_template,
                        "name": uri_template,
                    }))
                    .unwrap()
                })
                .collect(),
        }
    }

    #[test]
    fn lists_names_listed_uris_only() {
        let catalog = catalog(&["docs://guide.md"], &["notes://{id}"]);
        assert!(catalog.lists("docs://guide.md"));
        assert!(!catalog.lists("docs://guide.md:2"));
        assert!(!catalog.lists("notes://1"));
    }

    #[test]
    fn uris_match_templates_by_their_literal_parts() {
        let catalog = catalog(&[], &["log://app:{month}", "repo://{owner}/{name}/issues"]);
        assert!(catalog.matches_template("log://app:2024-10"));
        assert!(!catalog.matches_template("log://app"));
        assert!(catalog.matches_template("repo://stippi/code-assistant/issues"));
        assert!(!catalog.matches_template("repo://stippi/code-assistant/pulls"));
        assert!(!catalog.matches_template("other://x"));
    }

    #[test]
    fn listings_narrow_templates_by_the_prefix_too() {
        let catalog = catalog(&["docs://guide.md"], &["notes://{id}"]);
        let notes = catalog.render_listing("notes://12");
        assert!(notes.contains("notes://{id}"), "{notes}");
        assert!(!notes.contains("docs://guide.md"), "{notes}");

        let jira = catalog.render_listing("jira://");
        assert_eq!(
            jira,
            "The server lists no resources starting with 'jira://'."
        );
    }
}
