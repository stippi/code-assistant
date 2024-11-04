use crate::types::{FileSystemEntryType, FileTreeEntry, FileUpdate};
use anyhow::Result;
use ignore::WalkBuilder;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

            match self.entry_type {
                FileSystemEntryType::Directory => {
                    result.push('/');
                    // Add [...] for unexpanded directories that aren't empty
                    if !self.is_expanded {
                        result.push_str(" [...]");
                    }
                }
                FileSystemEntryType::File => {}
            }
            result.push('\n');
        }

        // Only show children if this directory is expanded
        if matches!(self.entry_type, FileSystemEntryType::Directory) && self.is_expanded {
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

    fn expand_directory(
        &self,
        path: &Path,
        entry: &mut FileTreeEntry,
        current_depth: usize,
        max_depth: usize,
    ) -> Result<()> {
        if current_depth >= max_depth {
            entry.is_expanded = false;
            return Ok(());
        }

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

        let walker = WalkBuilder::new(path)
            .max_depth(Some(1)) // Only immediate children
            .hidden(false)
            .git_ignore(true)
            .filter_entry(move |e| {
                let file_name = e.file_name().to_string_lossy();
                !default_ignore
                    .iter()
                    .any(|pattern| match glob::Pattern::new(pattern) {
                        Ok(pat) => pat.matches(&file_name),
                        Err(_) => file_name.contains(pattern),
                    })
            })
            .build();

        for result in walker {
            let dir_entry = result?;
            let entry_path = dir_entry.path();

            // Skip the directory itself
            if entry_path == path {
                continue;
            }

            let name = entry_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();

            let is_dir = entry_path.is_dir();
            let mut child_entry = FileTreeEntry {
                name,
                entry_type: if is_dir {
                    FileSystemEntryType::Directory
                } else {
                    FileSystemEntryType::File
                },
                children: HashMap::new(),
                is_expanded: false,
            };

            if is_dir {
                self.expand_directory(
                    entry_path, // Path ist jetzt schon ein &Path
                    &mut child_entry,
                    current_depth + 1,
                    max_depth,
                )?;
            }

            entry.children.insert(child_entry.name.clone(), child_entry);
        }

        entry.is_expanded = true;
        Ok(())
    }

    pub fn create_initial_tree(&self, max_depth: usize) -> Result<FileTreeEntry> {
        let mut root = FileTreeEntry {
            name: self
                .root_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("root")
                .to_string(),
            entry_type: FileSystemEntryType::Directory,
            children: HashMap::new(),
            is_expanded: true, // Root is always expanded
        };

        self.expand_directory(&self.root_dir, &mut root, 0, max_depth)?;
        Ok(root)
    }

    pub fn list_files(&self, path: &PathBuf, max_depth: Option<usize>) -> Result<FileTreeEntry> {
        let mut entry = FileTreeEntry {
            name: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string(),
            entry_type: if path.is_dir() {
                FileSystemEntryType::Directory
            } else {
                FileSystemEntryType::File
            },
            children: HashMap::new(),
            is_expanded: true,
        };

        if path.is_dir() {
            self.expand_directory(
                path.as_path(), // Konvertierung zu &Path
                &mut entry,
                0,
                max_depth.unwrap_or(usize::MAX),
            )?;
        }

        Ok(entry)
    }

    pub fn format_with_line_numbers(content: &str) -> String {
        content
            .lines()
            .enumerate()
            .map(|(i, line)| format!("{:>4} | {}", i + 1, line))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Applies FuleUpdates to a file
    pub fn apply_updates(&self, path: &Path, updates: &[FileUpdate]) -> Result<String> {
        let content = std::fs::read_to_string(path)?;
        let lines: Vec<&str> = content.lines().collect();

        // Validate the updates
        for update in updates {
            if update.start_line == 0 || update.end_line == 0 {
                anyhow::bail!("Line numbers must start at 1");
            }
            if update.start_line > update.end_line {
                anyhow::bail!("Start line must not be greater than end line");
            }
            if update.end_line > lines.len() {
                anyhow::bail!(
                    "End line {} exceeds file length {}",
                    update.end_line,
                    lines.len()
                );
            }
        }

        // Sort the updates by start_line in reverse order
        let mut sorted_updates = updates.to_vec();
        sorted_updates.sort_by(|a, b| b.start_line.cmp(&a.start_line));

        // Check if there are any overlapping updates
        for updates in sorted_updates.windows(2) {
            if updates[1].end_line >= updates[0].start_line {
                anyhow::bail!(
                    "Overlapping updates: lines {}-{} and {}-{}",
                    updates[1].start_line,
                    updates[1].end_line,
                    updates[0].start_line,
                    updates[0].end_line
                );
            }
        }

        // Apply the updates from bottom to top
        let mut result = lines.join("\n");
        for update in sorted_updates {
            let prefix = if update.start_line > 1 {
                // Take everything until the start line (including the line-break before the start line)
                let prefix_end = lines[..update.start_line - 1].join("\n").len();
                &result[..=prefix_end]
            } else {
                ""
            };

            let suffix = if update.end_line < lines.len() {
                // Take everything after the end line (including the line-break after the end line)
                let suffix_start = lines[..update.end_line].join("\n").len() + 1;
                &result[suffix_start..]
            } else {
                ""
            };

            result = format!("{}{}{}", prefix, update.new_content, suffix);
        }

        Ok(result)
    }
}
