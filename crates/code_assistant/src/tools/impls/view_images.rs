use crate::tools::core::{
    ImageData, Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use anyhow::{anyhow, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

/// Maximum image file size: 20 MB
const MAX_IMAGE_SIZE: usize = 20 * 1024 * 1024;

/// Supported image extensions and their MIME types.
fn mime_type_for_extension(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "tiff" | "tif" => Some("image/tiff"),
        _ => None,
    }
}

/// Resolve a path relative to the project root and verify it stays within bounds.
/// Returns the resolved absolute path.  Rejects absolute paths, `..` traversals
/// that escape the root, and symlinks pointing outside the project.
fn resolve_project_path(root_dir: &std::path::Path, rel_path: &std::path::Path) -> Result<PathBuf> {
    if rel_path.is_absolute() {
        anyhow::bail!("Absolute paths are not allowed");
    }

    let candidate = root_dir.join(rel_path);

    // Canonicalize to resolve symlinks and .. components.
    let canonical = candidate
        .canonicalize()
        .map_err(|e| anyhow!("Failed to resolve path '{}': {}", rel_path.display(), e))?;

    // Also canonicalize the root so both sides use physical paths.
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
pub struct ViewImagesInput {
    pub project: String,
    pub paths: Vec<String>,
}

// --------------------------------------------------------------------------
// Output
// --------------------------------------------------------------------------

/// Information about a single loaded image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedImage {
    pub path: String,
    pub media_type: String,
    pub base64_data: String,
    /// Original file size in bytes (before encoding).
    pub file_size: usize,
}

#[derive(Serialize, Deserialize)]
pub struct ViewImagesOutput {
    pub project: String,
    pub loaded_images: Vec<LoadedImage>,
    pub failed_images: Vec<(String, String)>,
}

// --------------------------------------------------------------------------
// Render
// --------------------------------------------------------------------------

impl Render for ViewImagesOutput {
    fn status(&self) -> String {
        if self.failed_images.is_empty() {
            format!("Loaded {} image(s)", self.loaded_images.len())
        } else {
            format!(
                "Loaded {} image(s), failed to load {} image(s)",
                self.loaded_images.len(),
                self.failed_images.len()
            )
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        let mut out = String::new();

        for (path, error) in &self.failed_images {
            out.push_str(&format!(
                "Failed to load '{}' in project '{}': {}\n",
                path, self.project, error
            ));
        }

        if !self.loaded_images.is_empty() {
            out.push_str("Successfully loaded the following image(s):\n");
            for img in &self.loaded_images {
                let size_display = if img.file_size >= 1024 * 1024 {
                    format!("{:.1} MB", img.file_size as f64 / (1024.0 * 1024.0))
                } else {
                    format!("{:.1} KB", img.file_size as f64 / 1024.0)
                };
                out.push_str(&format!(
                    "- {} ({}, {})\n",
                    img.path, img.media_type, size_display
                ));
            }
        }

        out
    }

    fn render_images(&self) -> Vec<ImageData> {
        self.loaded_images
            .iter()
            .map(|img| ImageData {
                media_type: img.media_type.clone(),
                base64_data: img.base64_data.clone(),
            })
            .collect()
    }
}

// --------------------------------------------------------------------------
// ToolResult
// --------------------------------------------------------------------------

impl ToolResult for ViewImagesOutput {
    fn is_success(&self) -> bool {
        // Partial success (some images loaded) is still success.
        // Only report failure if no images were loaded at all.
        !self.loaded_images.is_empty()
    }
}

// --------------------------------------------------------------------------
// Tool
// --------------------------------------------------------------------------

pub struct ViewImagesTool;

#[async_trait::async_trait]
impl Tool for ViewImagesTool {
    type Input = ViewImagesInput;
    type Output = ViewImagesOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "View image files in a project. Reads binary image files and provides them as image content ",
            "so you can see and analyze their visual content.\n",
            "\n",
            "Supported formats: PNG, JPEG, GIF, WebP, BMP, TIFF.\n"
        );

        ToolSpec {
            name: "view_images",
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project containing the image files"
                    },
                    "paths": {
                        "type": "array",
                        "description": "Paths to image files relative to the project root directory",
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
            ],
            hidden: false,
            title_template: Some("Viewing {paths}"),
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

        let mut loaded_images = Vec::new();
        let mut failed_images = Vec::new();
        let root_dir = explorer.root_dir();

        for path_str in &input.paths {
            let path = PathBuf::from(path_str);

            // Check extension
            let ext = match path.extension().and_then(|e| e.to_str()) {
                Some(e) => e.to_string(),
                None => {
                    failed_images.push((
                        path_str.clone(),
                        "File has no extension; cannot determine image format".into(),
                    ));
                    continue;
                }
            };

            let media_type = match mime_type_for_extension(&ext) {
                Some(mt) => mt.to_string(),
                None => {
                    failed_images.push((
                        path_str.clone(),
                        format!(
                            "Unsupported image format '.{ext}'. Supported: png, jpg, jpeg, gif, webp, bmp, tiff, tif"
                        ),
                    ));
                    continue;
                }
            };

            // Resolve and validate the path stays within the project root
            let full_path = match resolve_project_path(&root_dir, &path) {
                Ok(p) => p,
                Err(e) => {
                    failed_images.push((path_str.clone(), e.to_string()));
                    continue;
                }
            };

            // Read the raw bytes
            let bytes = match tokio::fs::read(&full_path).await {
                Ok(b) => b,
                Err(e) => {
                    failed_images.push((path_str.clone(), format!("Failed to read file: {e}")));
                    continue;
                }
            };

            // Size check
            if bytes.len() > MAX_IMAGE_SIZE {
                let size_mb = bytes.len() as f64 / (1024.0 * 1024.0);
                failed_images.push((
                    path_str.clone(),
                    format!(
                        "Image file is too large ({size_mb:.1} MB). Maximum supported size is {} MB",
                        MAX_IMAGE_SIZE / (1024 * 1024)
                    ),
                ));
                continue;
            }

            // Do a quick validation that the bytes look like a valid image
            if let Err(e) = image::guess_format(&bytes) {
                failed_images.push((
                    path_str.clone(),
                    format!("File does not appear to be a valid image: {e}"),
                ));
                continue;
            }

            let base64_data = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let file_size = bytes.len();

            loaded_images.push(LoadedImage {
                path: path_str.clone(),
                media_type,
                base64_data,
                file_size,
            });
        }

        Ok(ViewImagesOutput {
            project: input.project.clone(),
            loaded_images,
            failed_images,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mocks::ToolTestFixture;
    use crate::tools::core::ToolRegistry;

    #[tokio::test]
    async fn test_view_images_unsupported_extension() -> Result<()> {
        let registry = ToolRegistry::global();
        let tool = registry
            .get("view_images")
            .expect("view_images tool should be registered");

        let mut fixture =
            ToolTestFixture::with_files(vec![("data.bin".to_string(), "not an image".to_string())]);
        let mut context = fixture.context();

        let mut params = json!({
            "project": "test-project",
            "paths": ["data.bin"]
        });

        let result = tool.invoke(&mut context, &mut params).await?;
        assert!(!result.is_success());

        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);
        assert!(output.contains("Unsupported image format"));

        Ok(())
    }

    #[tokio::test]
    async fn test_view_images_missing_file() -> Result<()> {
        let registry = ToolRegistry::global();
        let tool = registry
            .get("view_images")
            .expect("view_images tool should be registered");

        let mut fixture = ToolTestFixture::with_files(vec![]);
        let mut context = fixture.context();

        let mut params = json!({
            "project": "test-project",
            "paths": ["nonexistent.png"]
        });

        let result = tool.invoke(&mut context, &mut params).await?;
        assert!(!result.is_success());

        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);
        assert!(output.contains("Failed to resolve path"));

        Ok(())
    }

    #[tokio::test]
    async fn test_view_images_render_images() -> Result<()> {
        // Create a minimal valid PNG (1x1 pixel)
        let png_data = create_minimal_png();
        let base64_png = base64::engine::general_purpose::STANDARD.encode(&png_data);

        let output = ViewImagesOutput {
            project: "test".to_string(),
            loaded_images: vec![LoadedImage {
                path: "icon.png".to_string(),
                media_type: "image/png".to_string(),
                base64_data: base64_png.clone(),
                file_size: png_data.len(),
            }],
            failed_images: vec![],
        };

        let images = output.render_images();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].media_type, "image/png");
        assert_eq!(images[0].base64_data, base64_png);

        Ok(())
    }

    #[tokio::test]
    async fn test_view_images_absolute_path_rejected() -> Result<()> {
        let registry = ToolRegistry::global();
        let tool = registry
            .get("view_images")
            .expect("view_images tool should be registered");

        let mut fixture = ToolTestFixture::with_files(vec![]);
        let mut context = fixture.context();

        let mut params = json!({
            "project": "test-project",
            "paths": ["/etc/image.png"]
        });

        let result = tool.invoke(&mut context, &mut params).await?;
        assert!(!result.is_success());

        let mut tracker = ResourcesTracker::new();
        let output = result.as_render().render(&mut tracker);
        assert!(output.contains("Absolute paths are not allowed"));

        Ok(())
    }

    #[tokio::test]
    async fn test_view_images_partial_success() -> Result<()> {
        let output = ViewImagesOutput {
            project: "test".to_string(),
            loaded_images: vec![LoadedImage {
                path: "good.png".to_string(),
                media_type: "image/png".to_string(),
                base64_data: "dGVzdA==".to_string(),
                file_size: 4,
            }],
            failed_images: vec![("bad.png".to_string(), "not found".to_string())],
        };

        // Partial success should still be reported as success
        assert!(output.is_success());

        let status = output.status();
        assert!(status.contains("1 image(s)"));
        assert!(status.contains("failed"));

        Ok(())
    }

    /// Create a minimal valid 1x1 white PNG file.
    fn create_minimal_png() -> Vec<u8> {
        use std::io::Cursor;
        let mut buf = Cursor::new(Vec::new());
        let encoder = image::codecs::png::PngEncoder::new(&mut buf);
        // 1x1 white pixel RGBA
        image::ImageEncoder::write_image(
            encoder,
            &[255, 255, 255, 255],
            1,
            1,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
        buf.into_inner()
    }
}
