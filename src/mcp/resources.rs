use super::types::{Resource, ResourceContent};
use crate::types::FileTreeEntry;
use std::collections::HashSet;

pub struct ResourceManager {
    file_tree: Option<FileTreeEntry>,
    subscriptions: HashSet<String>,
}

impl ResourceManager {
    pub fn new() -> Self {
        Self {
            file_tree: None,
            subscriptions: HashSet::new(),
        }
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

    /// Updates the file tree
    pub fn update_file_tree(&mut self, tree: FileTreeEntry) {
        self.file_tree = Some(tree);
    }
}
