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
        self.to_string_with_indent(0, "")
    }

    fn to_string_with_indent(&self, level: usize, prefix: &str) -> String {
        let mut result = String::new();

        // Root level doesn't get a prefix
        if level == 0 {
            result.push_str(&format!("{}/\n", self.name));
        } else {
            result.push_str(prefix);
            result.push_str(&self.name);
            if matches!(self.entry_type, FileSystemEntryType::Directory) {
                result.push('/');
            }
            result.push('\n');
        }

        // Sort children: directories first, then files, both alphabetically
        let mut sorted_children: Vec<_> = self.children.values().collect();
        sorted_children.sort_by_key(|entry| {
            (
                matches!(entry.entry_type, FileSystemEntryType::File),
                &entry.name,
            )
        });

        // Add children
        let child_count = sorted_children.len();
        for (i, child) in sorted_children.iter().enumerate() {
            let is_last = i == child_count - 1;

            // Construct the prefix for this child
            let child_prefix = if level == 0 {
                if is_last {
                    format!("└─ ")
                } else {
                    format!("├─ ")
                }
            } else {
                if is_last {
                    format!("{}└─ ", prefix.replace("├─ ", "│  ").replace("└─ ", "   "))
                } else {
                    format!("{}├─ ", prefix.replace("├─ ", "│  ").replace("└─ ", "   "))
                }
            };

            result.push_str(&child.to_string_with_indent(level + 1, &child_prefix));
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
            ".git",
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

            // Build path components
            let components: Vec<_> = relative_path.components().collect();

            // Start from root and traverse/build the tree structure
            let mut current = &mut root;
            for (i, component) in components.iter().enumerate() {
                let name = component.as_os_str().to_string_lossy().to_string();
                let is_last = i == components.len() - 1;

                // If this is not the last component, it must be a directory
                // If it is the last component, use the actual file type
                let entry_type = if !is_last {
                    FileSystemEntryType::Directory
                } else if path.is_file() {
                    FileSystemEntryType::File
                } else {
                    FileSystemEntryType::Directory
                };

                current = current
                    .children
                    .entry(name.clone())
                    .or_insert(FileTreeEntry {
                        name,
                        entry_type,
                        children: HashMap::new(),
                    });
            }
        }

        Ok(root)
    }
}
