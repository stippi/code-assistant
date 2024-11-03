use crate::types::{FileSystemEntryType, FileTreeEntry};
use anyhow::Result;
use ignore::WalkBuilder;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::debug;

/// Handles file system operations for code exploration
pub struct CodeExplorer {
    pub root_dir: PathBuf,
}

impl FileTreeEntry {
    /// Converts the file tree to a readable string representation
    pub fn to_string(&self) -> String {
        self.to_string_with_indent(0)
    }

    fn to_string_with_indent(&self, indent: usize) -> String {
        let mut result = String::new();
        let indent_str = "│   ".repeat(indent);
        let prefix = if indent == 0 { "" } else { "├── " };

        // Add current entry
        result.push_str(&format!("{}{}{}/\n", indent_str, prefix, self.name));

        // Sort children by directories first, then files, both alphabetically
        let mut sorted_children: Vec<_> = self.children.values().collect();
        sorted_children.sort_by_key(|entry| {
            (
                matches!(entry.entry_type, FileSystemEntryType::File),
                &entry.name,
            )
        });

        // Add children
        for (i, child) in sorted_children.iter().enumerate() {
            let is_last = i == sorted_children.len() - 1;
            let child_indent = if indent == 0 { 0 } else { indent + 1 };

            if is_last {
                // Replace the last ├── with └── for the last item
                let child_str = child
                    .to_string_with_indent(child_indent)
                    .replace("├──", "└──");
                result.push_str(&child_str);
            } else {
                result.push_str(&child.to_string_with_indent(child_indent));
            }
        }

        result
    }
}

impl CodeExplorer {
    /// Creates a new CodeExplorer instance
    ///
    /// # Arguments
    /// * `root_dir` - The root directory to explore
    pub fn new(root_dir: PathBuf) -> Self {
        Self { root_dir }
    }

    /// Reads the content of a file
    ///
    /// # Arguments
    /// * `path` - Path to the file to read
    ///
    /// # Returns
    /// * `Result<String>` - File content or an error
    pub fn read_file(&self, path: &PathBuf) -> Result<String> {
        debug!("Reading file: {}", path.display());
        Ok(std::fs::read_to_string(path)?)
    }

    /// Creates a complete file tree of the repository
    pub fn create_file_tree(&self) -> Result<FileTreeEntry> {
        let mut root = FileTreeEntry {
            name: self
                .root_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("root")
                .to_string(),
            entry_type: FileSystemEntryType::Directory,
            children: HashMap::new(),
        };

        // Always ignore some common directories
        let default_ignore = [
            "target",
            "node_modules",
            "build",
            "dist",
            ".idea",
            ".vscode",
            "*.pyc",
            "*.pyo",
            "*.class",
            ".DS_Store",
            "Thumbs.db",
        ];

        // Create walker that respects .gitignore
        let walker = WalkBuilder::new(&self.root_dir)
            .hidden(false) // Show hidden files unless ignored
            .git_ignore(true) // Use .gitignore if present
            .filter_entry(move |entry| {
                let file_name = entry.file_name().to_string_lossy();
                !default_ignore
                    .iter()
                    .any(|pattern| match glob::Pattern::new(pattern) {
                        Ok(pat) => pat.matches(&file_name),
                        Err(_) => file_name.contains(pattern),
                    })
            })
            .build();

        for result in walker {
            let entry = result?;
            let path = entry.path();

            // Skip the root directory itself
            if path == self.root_dir {
                continue;
            }

            let relative_path = path.strip_prefix(&self.root_dir)?;

            // Build the tree structure
            let mut current = &mut root;
            for component in relative_path.parent().unwrap_or(relative_path).components() {
                let name = component.as_os_str().to_string_lossy().to_string();
                current = current.children.entry(name).or_insert(FileTreeEntry {
                    name: component.as_os_str().to_string_lossy().to_string(),
                    entry_type: FileSystemEntryType::Directory,
                    children: HashMap::new(),
                });
            }

            // Add file entry
            if path.is_file() {
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                current.children.insert(
                    name.clone(),
                    FileTreeEntry {
                        name,
                        entry_type: FileSystemEntryType::File,
                        children: HashMap::new(),
                    },
                );
            }
        }

        Ok(root)
    }
}
