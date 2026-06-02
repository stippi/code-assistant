use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

/// Maximum document file size: 50 MB
const MAX_DOCUMENT_SIZE: usize = 50 * 1024 * 1024;

/// Maximum total markdown output length (characters) to avoid flooding the context window.
const MAX_OUTPUT_CHARS: usize = 150_000;

/// Supported document extensions and their human-readable format names.
fn document_format_for_extension(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "pdf" => Some("PDF"),
        "docx" => Some("DOCX"),
        "xlsx" => Some("XLSX"),
        "pptx" => Some("PPTX"),
        "odt" => Some("ODT"),
        "rtf" => Some("RTF"),
        "html" | "htm" => Some("HTML"),
        "xml" => Some("XML"),
        "csv" => Some("CSV"),
        "tsv" => Some("TSV"),
        _ => None,
    }
}

/// Resolve a path relative to the project root and verify it stays within bounds.
fn resolve_project_path(root_dir: &std::path::Path, rel_path: &std::path::Path) -> Result<PathBuf> {
    if rel_path.is_absolute() {
        anyhow::bail!("Absolute paths are not allowed");
    }

    let candidate = root_dir.join(rel_path);

    let canonical = candidate
        .canonicalize()
        .map_err(|e| anyhow!("Failed to resolve path '{}': {}", rel_path.display(), e))?;

    let canonical_root = root_dir
        .canonicalize()
        .unwrap_or_else(|_| root_dir.to_path_buf());

    if canonical.starts_with(&canonical_root) {
        Ok(canonical)
    } else {
        anyhow::bail!("Access outside project root is not allowed")
    }
}

// --------------------------------------------------------------------------
// Input
// --------------------------------------------------------------------------

#[derive(Deserialize, Serialize)]
pub struct ViewDocumentsInput {
    pub project: String,
    pub paths: Vec<String>,
}

// --------------------------------------------------------------------------
// Output
// --------------------------------------------------------------------------

/// A single successfully converted document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvertedDocument {
    pub path: String,
    pub format: String,
    pub markdown: String,
    pub page_count: usize,
    pub file_size: usize,
    /// Whether the markdown output was truncated due to length limits.
    pub truncated: bool,
}

#[derive(Serialize, Deserialize)]
pub struct ViewDocumentsOutput {
    pub project: String,
    pub converted_documents: Vec<ConvertedDocument>,
    pub failed_documents: Vec<(String, String)>,
}

// --------------------------------------------------------------------------
// Render
// --------------------------------------------------------------------------

impl Render for ViewDocumentsOutput {
    fn status(&self) -> String {
        if self.failed_documents.is_empty() {
            format!("Converted {} document(s)", self.converted_documents.len())
        } else {
            format!(
                "Converted {} document(s), failed {} document(s)",
                self.converted_documents.len(),
                self.failed_documents.len()
            )
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        let mut out = String::new();

        for (path, error) in &self.failed_documents {
            out.push_str(&format!(
                "Failed to convert '{}' in project '{}': {}\n",
                path, self.project, error
            ));
        }

        for doc in &self.converted_documents {
            let size_display = if doc.file_size >= 1024 * 1024 {
                format!("{:.1} MB", doc.file_size as f64 / (1024.0 * 1024.0))
            } else {
                format!("{:.1} KB", doc.file_size as f64 / 1024.0)
            };

            out.push_str(&format!(
                ">>>>> DOCUMENT: {} ({}, {}, {} pages)\n",
                doc.path, doc.format, size_display, doc.page_count
            ));
            out.push_str(&doc.markdown);
            if !doc.markdown.ends_with('\n') {
                out.push('\n');
            }
            if doc.truncated {
                out.push_str("[... output truncated due to length limits ...]\n");
            }
            out.push_str("<<<<< END DOCUMENT\n\n");
        }

        out
    }
}

// --------------------------------------------------------------------------
// ToolResult
// --------------------------------------------------------------------------

impl ToolResult for ViewDocumentsOutput {
    fn is_success(&self) -> bool {
        !self.converted_documents.is_empty()
    }
}

// --------------------------------------------------------------------------
// Tool
// --------------------------------------------------------------------------

pub struct ViewDocumentsTool;

#[async_trait::async_trait]
impl Tool for ViewDocumentsTool {
    type Input = ViewDocumentsInput;
    type Output = ViewDocumentsOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "View document files in a project by converting them to Markdown.\n",
            "Reads binary document files, converts their content to Markdown text, ",
            "and returns the result so you can read and analyze the document content.\n",
            "\n",
            "Supported formats: PDF, DOCX, XLSX, PPTX, ODT, RTF, HTML, XML, CSV, TSV.\n",
        );

        ToolSpec {
            name: "view_documents",
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project containing the document files"
                    },
                    "paths": {
                        "type": "array",
                        "description": "Paths to document files relative to the project root directory",
                        "items": {
                            "type": "string"
                        }
                    }
                },
                "required": ["project", "paths"]
            }),
            annotations: Some(json!({
                "readOnlyHint": true,
                "idempotentHint": true
            })),
            supported_scopes: &[
                ToolScope::McpServer,
                ToolScope::Agent,
                ToolScope::AgentWithDiffBlocks,
                ToolScope::SubAgentReadOnly,
                ToolScope::SubAgentDefault,
                ToolScope::SubAgentDefaultWithDiffBlocks,
            ],
            hidden: false,
            title_template: Some("Viewing documents {paths}"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        let explorer = context
            .project_manager
            .get_explorer_for_project(&input.project)
            .map_err(|e| {
                anyhow!(
                    "Failed to get explorer for project {}: {}",
                    input.project,
                    e
                )
            })?;

        let mut converted_documents = Vec::new();
        let mut failed_documents = Vec::new();
        let root_dir = explorer.root_dir();
        let mut total_output_chars: usize = 0;

        for path_str in &input.paths {
            let path = PathBuf::from(path_str);

            // Check extension
            let ext = match path.extension().and_then(|e| e.to_str()) {
                Some(e) => e.to_string(),
                None => {
                    failed_documents.push((
                        path_str.clone(),
                        "File has no extension; cannot determine document format".into(),
                    ));
                    continue;
                }
            };

            let format_name = match document_format_for_extension(&ext) {
                Some(name) => name.to_string(),
                None => {
                    failed_documents.push((
                        path_str.clone(),
                        format!(
                            "Unsupported document format '.{ext}'. Supported: pdf, docx, xlsx, pptx, odt, rtf, html, xml, csv, tsv"
                        ),
                    ));
                    continue;
                }
            };

            // Resolve and validate the path
            let full_path = match resolve_project_path(&root_dir, &path) {
                Ok(p) => p,
                Err(e) => {
                    failed_documents.push((path_str.clone(), e.to_string()));
                    continue;
                }
            };

            // Check file size before reading
            let metadata = match tokio::fs::metadata(&full_path).await {
                Ok(m) => m,
                Err(e) => {
                    failed_documents.push((
                        path_str.clone(),
                        format!("Failed to read file metadata: {e}"),
                    ));
                    continue;
                }
            };

            let file_size = metadata.len() as usize;
            if file_size > MAX_DOCUMENT_SIZE {
                let size_mb = file_size as f64 / (1024.0 * 1024.0);
                failed_documents.push((
                    path_str.clone(),
                    format!(
                        "Document file is too large ({size_mb:.1} MB). Maximum supported size is {} MB",
                        MAX_DOCUMENT_SIZE / (1024 * 1024)
                    ),
                ));
                continue;
            }

            // Convert the document using transmutation
            match convert_document(&full_path).await {
                Ok((markdown, page_count)) => {
                    let remaining_budget = MAX_OUTPUT_CHARS.saturating_sub(total_output_chars);
                    let truncated = markdown.len() > remaining_budget;
                    let final_markdown = if truncated {
                        // Truncate at a char boundary
                        let mut end = remaining_budget;
                        while end < markdown.len() && !markdown.is_char_boundary(end) {
                            end -= 1;
                        }
                        markdown[..end].to_string()
                    } else {
                        markdown
                    };

                    total_output_chars += final_markdown.len();

                    converted_documents.push(ConvertedDocument {
                        path: path_str.clone(),
                        format: format_name,
                        markdown: final_markdown,
                        page_count,
                        file_size,
                        truncated,
                    });
                }
                Err(e) => {
                    failed_documents.push((path_str.clone(), format!("Conversion failed: {e}")));
                }
            }
        }

        Ok(ViewDocumentsOutput {
            project: input.project.clone(),
            converted_documents,
            failed_documents,
        })
    }
}

// --------------------------------------------------------------------------
// Conversion helper (behind feature flag)
// --------------------------------------------------------------------------

#[cfg(feature = "document-conversion")]
async fn convert_document(path: &std::path::Path) -> Result<(String, usize)> {
    use transmutation::{Converter, OutputFormat};

    let converter =
        Converter::new().map_err(|e| anyhow!("Failed to initialize document converter: {e}"))?;

    let result = converter
        .convert(
            path.to_str()
                .ok_or_else(|| anyhow!("Invalid path encoding"))?,
        )
        .to(OutputFormat::Markdown {
            split_pages: false,
            optimize_for_llm: true,
        })
        .execute()
        .await
        .map_err(|e| anyhow!("Document conversion error: {e}"))?;

    let page_count = result.page_count();

    // Collect all content into a single markdown string.
    // ConversionOutput.data is Vec<u8> containing UTF-8 markdown text.
    let markdown: String = result
        .content
        .iter()
        .filter_map(|output| String::from_utf8(output.data.clone()).ok())
        .collect::<Vec<_>>()
        .join("\n\n");

    Ok((markdown, page_count))
}

#[cfg(not(feature = "document-conversion"))]
async fn convert_document(_path: &std::path::Path) -> Result<(String, usize)> {
    anyhow::bail!(
        "Document conversion is not available. \
         Build with `--features document-conversion` to enable this feature."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_document_format_detection() {
        assert_eq!(document_format_for_extension("pdf"), Some("PDF"));
        assert_eq!(document_format_for_extension("PDF"), Some("PDF"));
        assert_eq!(document_format_for_extension("docx"), Some("DOCX"));
        assert_eq!(document_format_for_extension("xlsx"), Some("XLSX"));
        assert_eq!(document_format_for_extension("pptx"), Some("PPTX"));
        assert_eq!(document_format_for_extension("odt"), Some("ODT"));
        assert_eq!(document_format_for_extension("rtf"), Some("RTF"));
        assert_eq!(document_format_for_extension("html"), Some("HTML"));
        assert_eq!(document_format_for_extension("htm"), Some("HTML"));
        assert_eq!(document_format_for_extension("xml"), Some("XML"));
        assert_eq!(document_format_for_extension("csv"), Some("CSV"));
        assert_eq!(document_format_for_extension("tsv"), Some("TSV"));
        assert_eq!(document_format_for_extension("txt"), None);
        assert_eq!(document_format_for_extension("rs"), None);
    }

    #[test]
    fn test_output_rendering() {
        let output = ViewDocumentsOutput {
            project: "test".to_string(),
            converted_documents: vec![ConvertedDocument {
                path: "report.pdf".to_string(),
                format: "PDF".to_string(),
                markdown: "# Report\n\nThis is the content.".to_string(),
                page_count: 3,
                file_size: 50_000,
                truncated: false,
            }],
            failed_documents: vec![],
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        assert!(rendered.contains(">>>>> DOCUMENT: report.pdf"));
        assert!(rendered.contains("PDF"));
        assert!(rendered.contains("3 pages"));
        assert!(rendered.contains("# Report"));
        assert!(rendered.contains("<<<<< END DOCUMENT"));
    }

    #[test]
    fn test_output_with_failures() {
        let output = ViewDocumentsOutput {
            project: "test".to_string(),
            converted_documents: vec![],
            failed_documents: vec![("bad.xyz".to_string(), "Unsupported format".to_string())],
        };

        assert!(!output.is_success());

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);
        assert!(rendered.contains("Failed to convert 'bad.xyz'"));
        assert!(rendered.contains("Unsupported format"));
    }

    #[test]
    fn test_truncation_rendering() {
        let output = ViewDocumentsOutput {
            project: "test".to_string(),
            converted_documents: vec![ConvertedDocument {
                path: "big.pdf".to_string(),
                format: "PDF".to_string(),
                markdown: "Truncated content".to_string(),
                page_count: 100,
                file_size: 10_000_000,
                truncated: true,
            }],
            failed_documents: vec![],
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);
        assert!(rendered.contains("[... output truncated due to length limits ...]"));
    }

    #[tokio::test]
    async fn test_unsupported_extension() {
        use crate::tests::mocks::ToolTestFixture;
        use crate::tools::core::ToolRegistry;

        let registry = ToolRegistry::global();
        let tool = registry.get("view_documents");
        if tool.is_none() {
            // Tool not registered (feature disabled) - skip test
            return;
        }
        let tool = tool.unwrap();

        let mut fixture = ToolTestFixture::with_files(vec![(
            "data.bin".to_string(),
            "not a document".to_string(),
        )]);
        let mut context = fixture.context();

        let mut params = json!({
            "project": "test-project",
            "paths": ["data.bin"]
        });

        let result = tool.invoke(&mut context, &mut params).await.unwrap();
        assert!(!result.is_success());

        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);
        assert!(output.contains("Unsupported document format"));
    }

    #[cfg(not(feature = "document-conversion"))]
    #[tokio::test]
    async fn test_feature_disabled_error() {
        let path = std::path::Path::new("/tmp/test.pdf");
        let result = convert_document(path).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not available"));
    }
}
