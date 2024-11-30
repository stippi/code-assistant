use crate::mcp::types::{Resource, ResourceContent};
use crate::types::FileTreeEntry;
use crate::utils::format_with_line_numbers;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Default)]
pub struct ResourceManager {
    loaded_files: Arc<RwLock<HashMap<PathBuf, String>>>,
    file_summaries: Arc<RwLock<HashMap<PathBuf, String>>>,
    file_tree: Arc<RwLock<Option<FileTreeEntry>>>,
}

impl ResourceManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Converts a path to a resource URI
    fn path_to_uri(&self, path: &Path) -> String {
        format!("file://{}", path.display())
    }

    /// Converts a path to a summary URI
    fn path_to_summary_uri(&self, path: &Path) -> String {
        format!("summary://{}", path.display())
    }

    /// Lists all available resources
    pub async fn list_resources(&self) -> Vec<Resource> {
        let mut resources = Vec::new();

        // Add file tree resource if available
        if self.file_tree.read().await.is_some() {
            resources.push(Resource {
                uri: "tree:///".to_string(),
                name: "Repository Structure".to_string(),
                description: Some("The repository file tree structure".to_string()),
                mime_type: Some("text/plain".to_string()),
            });
        }

        // Add loaded files
        let loaded_files = self.loaded_files.read().await;
        for path in loaded_files.keys() {
            resources.push(Resource {
                uri: self.path_to_uri(path),
                name: path.display().to_string(),
                description: Some("Source file content with line numbers".to_string()),
                mime_type: Some("text/plain".to_string()),
            });
        }

        // Add file summaries
        let summaries = self.file_summaries.read().await;
        for path in summaries.keys() {
            resources.push(Resource {
                uri: self.path_to_summary_uri(path),
                name: format!("Summary of {}", path.display()),
                description: Some("File content summary".to_string()),
                mime_type: Some("text/plain".to_string()),
            });
        }

        resources
    }

    /// Reads a specific resource content
    pub async fn read_resource(&self, uri: &str) -> Option<ResourceContent> {
        match uri {
            "tree:///" => {
                let tree = self.file_tree.read().await;
                tree.as_ref().map(|t| ResourceContent {
                    uri: uri.to_string(),
                    mime_type: Some("text/plain".to_string()),
                    text: Some(t.to_string()),
                })
            }
            _ if uri.starts_with("file://") => {
                let path = PathBuf::from(uri.strip_prefix("file://")?);
                let files = self.loaded_files.read().await;
                files.get(&path).map(|content| ResourceContent {
                    uri: uri.to_string(),
                    mime_type: Some("text/plain".to_string()),
                    text: Some(format_with_line_numbers(content)),
                })
            }
            _ if uri.starts_with("summary://") => {
                let path = PathBuf::from(uri.strip_prefix("summary://")?);
                let summaries = self.file_summaries.read().await;
                summaries.get(&path).map(|summary| ResourceContent {
                    uri: uri.to_string(),
                    mime_type: Some("text/plain".to_string()),
                    text: Some(summary.clone()),
                })
            }
            _ => None,
        }
    }

    /// Updates the file tree
    pub async fn update_file_tree(&self, tree: FileTreeEntry) {
        let mut file_tree = self.file_tree.write().await;
        *file_tree = Some(tree);
    }

    /// Adds or updates a loaded file
    pub async fn update_loaded_file(&self, path: PathBuf, content: String) {
        let mut files = self.loaded_files.write().await;
        files.insert(path, content);
    }

    /// Adds or updates a file summary
    pub async fn update_file_summary(&self, path: PathBuf, summary: String) {
        let mut summaries = self.file_summaries.write().await;
        summaries.insert(path, summary);
    }
}

// Add Clone implementation for ResourceManager
impl Clone for ResourceManager {
    fn clone(&self) -> Self {
        Self {
            loaded_files: self.loaded_files.clone(),
            file_summaries: self.file_summaries.clone(),
            file_tree: self.file_tree.clone(),
        }
    }
}
