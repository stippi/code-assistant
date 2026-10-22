//! The `:mcp:<server>` scope of the file tools: an MCP server's resources,
//! addressed by URI instead of path.

use crate::tools::ToolServicesAccess;
use crate::tools::core::ToolContext;
use mcp_client::{ResourceCatalog, ResourceServer};

/// The server an `:mcp:<server>` scope names, as offered to this session.
pub(crate) fn resource_server<'c>(
    context: &'c ToolContext<'_>,
    server: &str,
) -> Result<&'c ResourceServer, String> {
    let servers = context
        .mcp_resources()
        .ok_or_else(|| "MCP resources are not available here".to_string())?;
    servers.get(server).ok_or_else(|| {
        let offered: Vec<String> = servers
            .iter()
            .map(|(name, _)| format!(":mcp:{name}"))
            .collect();
        if offered.is_empty() {
            format!("no MCP server '{server}' offers resources in this session")
        } else {
            format!(
                "no MCP server '{server}' offers resources in this session (available: {})",
                offered.join(", ")
            )
        }
    })
}

/// A requested resource: its URI and an optional 1-indexed, inclusive line
/// range.
#[derive(Debug, PartialEq)]
pub(crate) struct ResourceRequest<'a> {
    pub uri: &'a str,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
}

impl<'a> ResourceRequest<'a> {
    /// Split a trailing line range (`:10-20`, `:10-`, `:-20`, `:15`) off a
    /// URI. Only when what remains still has a scheme: `file:///a.md:10-20`
    /// is lines 10–20 of `file:///a.md`, while `issue:123` stays a URI.
    ///
    /// What the server offers decides doubtful cases: a listed whole stays
    /// whole, a listed remainder takes the range, and so does a URI only one
    /// of the server's templates produces whole (`log://app:{month}`).
    /// Otherwise a bare number right after an authority (`http://host:8080`)
    /// is taken for a port.
    pub fn parse(path: &'a str, catalog: &ResourceCatalog) -> Self {
        let whole = Self::whole(path);
        let Some((uri, range)) = path.rsplit_once(':') else {
            return whole;
        };
        if catalog.lists(path)
            || !uri.contains(':')
            || !range.chars().any(|c| c.is_ascii_digit())
            || !range.chars().all(|c| c.is_ascii_digit() || c == '-')
        {
            return whole;
        }
        if !catalog.lists(uri) {
            if catalog.matches_template(path) && !catalog.matches_template(uri) {
                return whole;
            }
            let looks_like_port = !range.contains('-')
                && uri
                    .split_once("://")
                    .is_some_and(|(_, rest)| !rest.contains('/'));
            if looks_like_port {
                return whole;
            }
        }
        let number = |part: &str| -> Result<Option<usize>, ()> {
            if part.is_empty() {
                Ok(None)
            } else {
                part.parse().map(Some).map_err(|_| ())
            }
        };
        let parsed = match range.split_once('-') {
            Some((start, end)) => number(start).and_then(|start| Ok((start, number(end)?))),
            None => number(range).map(|line| (line, line)),
        };
        match parsed {
            Ok((start_line, end_line)) => Self {
                uri,
                start_line,
                end_line,
            },
            Err(()) => whole,
        }
    }

    /// The whole resource at `uri`.
    pub fn whole(uri: &'a str) -> Self {
        Self {
            uri,
            start_line: None,
            end_line: None,
        }
    }

    pub fn has_line_range(&self) -> bool {
        self.start_line.is_some() || self.end_line.is_some()
    }

    /// The requested lines of `text`.
    pub fn select_lines(&self, text: &str) -> Result<String, String> {
        if !self.has_line_range() {
            return Ok(text.to_string());
        }
        let lines: Vec<&str> = text.lines().collect();
        let start = self.start_line.unwrap_or(1).max(1);
        let end = self.end_line.unwrap_or(lines.len()).min(lines.len());
        if start > lines.len() {
            return Err(format!(
                "line {start} is past the end of the resource ({} lines)",
                lines.len()
            ));
        }
        if start > end {
            return Err(format!("invalid line range {start}-{end}"));
        }
        Ok(lines[start - 1..end].join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        uri: &str,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> ResourceRequest<'_> {
        ResourceRequest {
            uri,
            start_line,
            end_line,
        }
    }

    /// A catalog listing `resources` and offering `templates`.
    fn catalog(resources: &[&str], templates: &[&str]) -> ResourceCatalog {
        ResourceCatalog {
            resources: resources
                .iter()
                .map(|uri| rmcp::model::Resource::new(*uri, *uri))
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
    fn line_ranges_split_off_uris_with_a_scheme() {
        let empty = catalog(&[], &[]);
        assert_eq!(
            ResourceRequest::parse("file:///a.md:10-20", &empty),
            request("file:///a.md", Some(10), Some(20))
        );
        assert_eq!(
            ResourceRequest::parse("file:///a.md:10-", &empty),
            request("file:///a.md", Some(10), None)
        );
        assert_eq!(
            ResourceRequest::parse("file:///a.md:-20", &empty),
            request("file:///a.md", None, Some(20))
        );
        assert_eq!(
            ResourceRequest::parse("file:///a.md:15", &empty),
            request("file:///a.md", Some(15), Some(15))
        );
        // Without a path, a range is still a range…
        assert_eq!(
            ResourceRequest::parse("docs://guide.md:2-3", &empty),
            request("docs://guide.md", Some(2), Some(3))
        );
        // …and a single line one when the server listed what precedes it.
        assert_eq!(
            ResourceRequest::parse("docs://guide.md:2", &catalog(&["docs://guide.md"], &[])),
            request("docs://guide.md", Some(2), Some(2))
        );
    }

    #[test]
    fn uris_without_a_range_stay_whole() {
        let empty = catalog(&[], &[]);
        for uri in [
            "file:///a.md",
            "issue:123",
            "jira://PROJ-123",
            "http://host:8080",
        ] {
            let parsed = ResourceRequest::parse(uri, &empty);
            assert_eq!(parsed.uri, uri, "{uri}");
            assert!(!parsed.has_line_range(), "{uri}");
        }
    }

    #[test]
    fn listed_uris_stay_whole() {
        let parsed = ResourceRequest::parse("file:///a.md:15", &catalog(&["file:///a.md:15"], &[]));
        assert_eq!(parsed.uri, "file:///a.md:15");
        assert!(!parsed.has_line_range());
    }

    #[test]
    fn uris_only_a_template_produces_whole_stay_whole() {
        let logs = catalog(&[], &["log://app:{month}"]);
        let parsed = ResourceRequest::parse("log://app:2024-10", &logs);
        assert_eq!(parsed.uri, "log://app:2024-10");
        assert!(!parsed.has_line_range());

        // A template producing both readings leaves the range a range.
        assert_eq!(
            ResourceRequest::parse("docs://guide.md:2-3", &catalog(&[], &["docs://{name}"])),
            request("docs://guide.md", Some(2), Some(3))
        );
    }

    #[test]
    fn line_selection() {
        let text = "one\ntwo\nthree";
        assert_eq!(
            request("x", Some(2), None).select_lines(text).unwrap(),
            "two\nthree"
        );
        assert_eq!(
            request("x", None, Some(1)).select_lines(text).unwrap(),
            "one"
        );
        assert!(request("x", Some(4), None).select_lines(text).is_err());
    }

    /// End to end through the file tools: an in-process MCP server offering
    /// resources, registered the way the app registers it.
    mod scope {
        use crate::mocks::ToolTestFixture;
        use crate::tools::core::{ResourcesTracker, ToolRegistry};
        use mcp_client::{McpResourceServers, McpServerConnection};
        use rmcp::model::{
            ErrorData, ListResourcesResult, PaginatedRequestParams, ReadResourceRequestParams,
            ReadResourceResponse, ReadResourceResult, Resource, ResourceContents,
            ServerCapabilities, ServerInfo,
        };
        use rmcp::service::{RequestContext, RoleServer};
        use rmcp::{ServerHandler, ServiceExt};
        use serde_json::json;
        use std::sync::Arc;

        const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

        struct Docs;

        impl ServerHandler for Docs {
            fn get_info(&self) -> ServerInfo {
                ServerInfo::new(ServerCapabilities::builder().enable_resources().build())
            }

            async fn list_resources(
                &self,
                _request: Option<PaginatedRequestParams>,
                _context: RequestContext<RoleServer>,
            ) -> Result<ListResourcesResult, ErrorData> {
                Ok(ListResourcesResult::with_all_items(vec![
                    Resource::new("docs://guide.md", "guide.md").with_description("The guide"),
                    Resource::new("docs://logo.png", "logo.png"),
                    Resource::new("other://notes", "notes"),
                ]))
            }

            async fn read_resource(
                &self,
                request: ReadResourceRequestParams,
                _context: RequestContext<RoleServer>,
            ) -> Result<ReadResourceResponse, ErrorData> {
                let contents = match request.uri.as_str() {
                    "docs://guide.md" => {
                        vec![ResourceContents::text("one\ntwo\nthree", "docs://guide.md")]
                    }
                    "docs://both" => vec![
                        ResourceContents::text("a1\na2", "docs://both/a"),
                        ResourceContents::text("b1\nb2", "docs://both/b"),
                    ],
                    "docs://archive.zip" => vec![ResourceContents::BlobResourceContents {
                        uri: String::new(),
                        mime_type: Some("application/zip".into()),
                        blob: "//4A".into(), // 0xff 0xfe 0x00: not UTF-8
                        meta: None,
                    }],
                    // Not listed, and its tail looks like a line range.
                    "urn:docs:42" => vec![ResourceContents::text("answer", "urn:docs:42")],
                    "docs://logo.png" => vec![ResourceContents::BlobResourceContents {
                        uri: "docs://logo.png".into(),
                        mime_type: Some("image/png".into()),
                        blob: PNG_1X1.into(),
                        meta: None,
                    }],
                    other => {
                        return Err(ErrorData::resource_not_found(
                            format!("no resource {other}"),
                            None,
                        ));
                    }
                };
                Ok(ReadResourceResult::new(contents).into())
            }
        }

        /// The resource servers a registry build leaves for the session.
        async fn docs_servers() -> Arc<McpResourceServers> {
            let (client_io, server_io) = tokio::io::duplex(64 * 1024);
            tokio::spawn(async move {
                let service = Docs.serve(server_io).await.expect("server starts");
                let _ = service.waiting().await;
            });
            let connection = McpServerConnection::connect_transport("docs", client_io)
                .await
                .expect("client connects");
            let mut registry = ToolRegistry::new();
            let config = serde_json::from_value(json!({ "command": "unused" })).unwrap();
            mcp_client::register_connection_tools(
                &mut registry,
                Arc::new(connection),
                &config,
                &[],
            )
            .await
            .unwrap();
            crate::tools::mcp::session_resource_servers(&registry, &[])
                .expect("docs offers resources")
        }

        async fn invoke(
            tool: &str,
            servers: Arc<McpResourceServers>,
            mut params: serde_json::Value,
        ) -> (bool, String, usize) {
            let registry = crate::tools::test_registry();
            let mut fixture = ToolTestFixture::new().with_mcp_resources(servers);
            let mut context = fixture.context();
            let output = registry
                .get(tool)
                .unwrap()
                .invoke(&mut context, &mut params)
                .await
                .unwrap();
            let render = output.as_render();
            (
                output.is_success(),
                render.render(&mut ResourcesTracker::new()),
                render.render_images().len(),
            )
        }

        #[tokio::test]
        async fn read_files_reads_resources_with_line_ranges_and_images() {
            let servers = docs_servers().await;

            let (ok, text, _) = invoke(
                "read_files",
                servers.clone(),
                json!({ "project": ":mcp:docs", "paths": ["docs://guide.md:2-3"] }),
            )
            .await;
            assert!(ok, "{text}");
            assert!(
                text.contains(">>>>> FILE: docs://guide.md:2-3\ntwo\nthree\n<<<<< END FILE"),
                "{text}"
            );

            let (ok, text, images) = invoke(
                "read_files",
                servers.clone(),
                json!({ "project": ":mcp:docs", "paths": ["docs://logo.png"] }),
            )
            .await;
            assert!(ok, "{text}");
            assert_eq!(images, 1);

            let (ok, text, _) = invoke(
                "read_files",
                servers,
                json!({ "project": ":mcp:docs", "paths": ["docs://missing"] }),
            )
            .await;
            assert!(!ok);
            assert!(text.contains("no resource docs://missing"), "{text}");
        }

        #[tokio::test]
        async fn each_part_of_a_resource_is_its_own_file_with_the_range_applied() {
            let (ok, text, _) = invoke(
                "read_files",
                docs_servers().await,
                json!({ "project": ":mcp:docs", "paths": ["docs://both:2-2"] }),
            )
            .await;
            assert!(ok, "{text}");
            assert!(
                text.contains(">>>>> FILE: docs://both/a:2-2\na2\n<<<<< END FILE"),
                "{text}"
            );
            assert!(
                text.contains(">>>>> FILE: docs://both/b:2-2\nb2\n<<<<< END FILE"),
                "{text}"
            );
        }

        #[tokio::test]
        async fn unusable_content_fails_under_the_requested_path() {
            let (ok, text, _) = invoke(
                "read_files",
                docs_servers().await,
                json!({ "project": ":mcp:docs", "paths": ["docs://archive.zip"] }),
            )
            .await;
            assert!(!ok);
            assert!(
                text.contains(
                    "Failed to load 'docs://archive.zip' in project ':mcp:docs': binary content"
                ),
                "{text}"
            );
        }

        #[tokio::test]
        async fn an_unlisted_uri_misread_as_a_range_is_read_whole() {
            let (ok, text, _) = invoke(
                "read_files",
                docs_servers().await,
                json!({ "project": ":mcp:docs", "paths": ["urn:docs:42"] }),
            )
            .await;
            assert!(ok, "{text}");
            assert!(
                text.contains(">>>>> FILE: urn:docs:42\nanswer\n<<<<< END FILE"),
                "{text}"
            );
        }

        #[tokio::test]
        async fn unknown_or_unoffered_servers_fail_per_path() {
            let (ok, text, _) = invoke(
                "read_files",
                docs_servers().await,
                json!({ "project": ":mcp:jira", "paths": ["jira://x"] }),
            )
            .await;
            assert!(!ok);
            assert!(text.contains("available: :mcp:docs"), "{text}");

            // A session that switched the server off does not reach it.
            let registry = crate::tools::test_registry();
            let mut fixture = ToolTestFixture::new();
            let mut context = fixture.context();
            let mut params = json!({ "project": ":mcp:docs", "paths": ["docs://guide.md"] });
            let output = registry
                .get("read_files")
                .unwrap()
                .invoke(&mut context, &mut params)
                .await
                .unwrap();
            let text = output.as_render().render(&mut ResourcesTracker::new());
            assert!(
                text.contains("MCP resources are not available here"),
                "{text}"
            );
        }

        #[tokio::test]
        async fn list_files_lists_resources_by_uri_prefix() {
            let servers = docs_servers().await;

            let (ok, all, _) = invoke(
                "list_files",
                servers.clone(),
                json!({ "project": ":mcp:docs", "paths": ["."] }),
            )
            .await;
            assert!(ok, "{all}");
            assert!(
                all.contains("- docs://guide.md — guide.md: The guide"),
                "{all}"
            );
            assert!(all.contains("- other://notes — notes"), "{all}");

            let (_, narrowed, _) = invoke(
                "list_files",
                servers,
                json!({ "project": ":mcp:docs", "paths": ["docs://"] }),
            )
            .await;
            assert!(narrowed.contains("docs://logo.png"), "{narrowed}");
            assert!(!narrowed.contains("other://notes"), "{narrowed}");
        }

        #[tokio::test]
        async fn the_prompt_names_each_servers_resources_where_read_files_is_offered() {
            use crate::tools::core::ToolScope;
            use crate::tools::mcp::resources_prompt_section;

            let servers = docs_servers().await;
            let mut registry = ToolRegistry::new();
            crate::tools::register_default_tools(&mut registry, &Default::default());
            *registry.extension_mut::<McpResourceServers>() = (*servers).clone();

            let section = resources_prompt_section(&registry, ToolScope::Agent, &[]).unwrap();
            assert!(section.starts_with("# MCP Resources\n\n"), "{section}");
            assert!(section.contains("## :mcp:docs"), "{section}");
            assert!(
                section.contains("- docs://guide.md — guide.md: The guide"),
                "{section}"
            );
            assert!(
                resources_prompt_section(&registry, ToolScope::Agent, &["docs".to_string()])
                    .is_none(),
                "a switched-off server is not offered"
            );
            assert!(
                resources_prompt_section(&registry, ToolScope::Custom("scope:memory"), &[])
                    .is_none(),
                "a scope without read_files gets no section"
            );
        }
    }
}
