use super::types::{Resource, ResourceContent};
use crate::types::FileTreeEntry;
use crate::utils::format_with_line_numbers;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct ResourceManager {
    loaded_files: HashMap<PathBuf, String>,
    file_summaries: HashMap<PathBuf, String>,
    file_tree: Option<FileTreeEntry>,
    subscriptions: HashSet<String>,
}

impl ResourceManager {
    pub fn new() -> Self {
        Self {
            loaded_files: HashMap::new(),
            file_summaries: HashMap::new(),
            file_tree: None,
            subscriptions: HashSet::new(),
        }
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
    pub fn list_resources(&self) -> Vec<Resource> {
        let mut resources = Vec::new();

        // Add file tree resource if available
        if self.file_tree.is_some() {
            resources.push(Resource {
                uri: "tree:///".to_string(),
                name: "Repository Structure".to_string(),
                description: Some("The repository file tree structure".to_string()),
                mime_type: Some("text/plain".to_string()),
            });
        }

        // Add loaded files
        for path in self.loaded_files.keys() {
            resources.push(Resource {
                uri: self.path_to_uri(path),
                name: path.display().to_string(),
                description: Some("Source file content with line numbers".to_string()),
                mime_type: Some("text/plain".to_string()),
            });
        }

        // Add file summaries
        for path in self.file_summaries.keys() {
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
    pub fn read_resource(&self, uri: &str) -> Option<ResourceContent> {
        match uri {
            "tree:///" => self.file_tree.as_ref().map(|t| ResourceContent {
                uri: uri.to_string(),
                mime_type: Some("text/plain".to_string()),
                text: Some(t.to_string()),
            }),
            _ if uri.starts_with("file://") => {
                let path = PathBuf::from(uri.strip_prefix("file://")?);
                self.loaded_files.get(&path).map(|content| ResourceContent {
                    uri: uri.to_string(),
                    mime_type: Some("text/plain".to_string()),
                    text: Some(format_with_line_numbers(content)),
                })
            }
            _ if uri.starts_with("summary://") => {
                let path = PathBuf::from(uri.strip_prefix("summary://")?);
                self.file_summaries
                    .get(&path)
                    .map(|summary| ResourceContent {
                        uri: uri.to_string(),
                        mime_type: Some("text/plain".to_string()),
                        text: Some(summary.clone()),
                    })
            }
            _ => None,
        }
    }

    /// Subscribes to a resource
    pub fn subscribe(&mut self, uri: &str) {
        self.subscriptions.insert(uri.to_string());
    }

    /// Unsubscribes from a resource
    pub fn unsubscribe(&mut self, uri: &str) {
        self.subscriptions.remove(uri);
    }

    /// Checks if a resource is subscribed
    pub fn is_subscribed(&self, uri: &str) -> bool {
        self.subscriptions.contains(uri)
    }

    /// Remove a file summary
    pub fn remove_file_summary(&mut self, path: &PathBuf) {
        self.file_summaries.remove(path);
    }

    /// Updates the file tree
    pub fn update_file_tree(&mut self, tree: FileTreeEntry) {
        self.file_tree = Some(tree);
    }

    /// Adds or updates a loaded file
    pub fn update_loaded_file(&mut self, path: PathBuf, content: String) {
        self.loaded_files.insert(path, content);
    }

    /// Adds or updates a file summary
    pub fn update_file_summary(&mut self, path: PathBuf, summary: String) {
        self.file_summaries.insert(path, summary);
    }

    /// Check if a file is currently loaded
    pub fn is_file_loaded(&self, path: &PathBuf) -> bool {
        self.loaded_files.contains_key(path)
    }

    /// Removes a loaded file from working memory
    pub fn remove_loaded_file(&mut self, path: &PathBuf) {
        self.loaded_files.remove(path);
    }
}
