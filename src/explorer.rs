use crate::types::{CodeExplorer, FileSystemEntryType, FileTreeEntry, FileUpdate};
use anyhow::Result;
use ignore::WalkBuilder;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::debug;

/// Handles file system operations for code exploration
pub struct Explorer {
    root_dir: PathBuf,
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

impl Explorer {
    /// Creates a new Explorer instance
    ///
    /// # Arguments
    /// * `root_dir` - The root directory to explore
    pub fn new(root_dir: PathBuf) -> Self {
        Self { root_dir }
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
}

impl CodeExplorer for Explorer {
    fn root_dir(&self) -> PathBuf {
        self.root_dir.clone()
    }

    fn create_initial_tree(&self, max_depth: usize) -> Result<FileTreeEntry> {
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

    fn read_file(&self, path: &PathBuf) -> Result<String> {
        debug!("Reading file: {}", path.display());
        Ok(std::fs::read_to_string(path)?)
    }

    fn list_files(&self, path: &PathBuf, max_depth: Option<usize>) -> Result<FileTreeEntry> {
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

    fn apply_updates(&self, path: &Path, updates: &[FileUpdate]) -> Result<String> {
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
        let mut result = content.clone(); // Keep the original line breaks
        for update in sorted_updates {
            let start_index = if update.start_line > 1 {
                // Find the position after the previous line's newline
                result
                    .split('\n')
                    .take(update.start_line - 1)
                    .map(|line| line.len() + 1) // +1 for the newline
                    .sum()
            } else {
                0
            };

            let end_index = result
                .split('\n')
                .take(update.end_line)
                .map(|line| line.len() + 1)
                .sum::<usize>()
                - if update.end_line == lines.len() { 1 } else { 0 };

            // Make sure the new content ends in a line break unless it is at the end of the file
            let mut new_content = update.new_content.clone();
            if update.end_line < lines.len() && !new_content.ends_with('\n') {
                new_content.push('\n');
            }

            result.replace_range(start_index..end_index, &new_content);
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::format_with_line_numbers;
    use anyhow::Result;
    use std::fs;
    use tempfile::TempDir;

    // Helper function to setup temporary test environment
    fn setup_test_directory() -> Result<(TempDir, Explorer)> {
        let temp_dir = TempDir::new()?;
        let explorer = Explorer::new(temp_dir.path().to_path_buf());
        Ok((temp_dir, explorer))
    }

    // Helper function to create a test file with content
    fn create_test_file(dir: &Path, name: &str, content: &str) -> Result<PathBuf> {
        let file_path = dir.join(name);
        fs::write(&file_path, content)?;
        Ok(file_path)
    }

    #[test]
    fn test_read_file() -> Result<()> {
        let (temp_dir, explorer) = setup_test_directory()?;
        let test_content = "Hello, World!";
        let file_path = create_test_file(temp_dir.path(), "test.txt", test_content)?;

        let result = explorer.read_file(&file_path)?;
        assert_eq!(result, test_content);
        Ok(())
    }

    #[test]
    fn test_format_with_line_numbers() {
        let input = "First line\nSecond line\nThird line";
        let expected = "   1 | First line\n   2 | Second line\n   3 | Third line";

        assert_eq!(format_with_line_numbers(input), expected);
    }

    #[test]
    fn test_apply_updates_single() -> Result<()> {
        let (temp_dir, explorer) = setup_test_directory()?;
        let initial_content = "Line 1\nLine 2\nLine 3\nLine 4\n";
        let file_path = create_test_file(temp_dir.path(), "test.txt", initial_content)?;

        let updates = vec![FileUpdate {
            start_line: 2,
            end_line: 3,
            new_content: "Updated Line 2\nUpdated Line 3".to_string(),
        }];

        let result = explorer.apply_updates(&file_path, &updates)?;
        assert_eq!(result, "Line 1\nUpdated Line 2\nUpdated Line 3\nLine 4\n");
        Ok(())
    }

    #[test]
    fn test_apply_updates_multiple() -> Result<()> {
        let (temp_dir, explorer) = setup_test_directory()?;
        let initial_content = "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\n";
        let file_path = create_test_file(temp_dir.path(), "test.txt", initial_content)?;

        let updates = vec![
            FileUpdate {
                start_line: 1,
                end_line: 2,
                new_content: "Updated Line 1\nUpdated Line 2".to_string(),
            },
            FileUpdate {
                start_line: 4,
                end_line: 5,
                new_content: "Updated Line 4\nUpdated Line 5".to_string(),
            },
        ];

        let result = explorer.apply_updates(&file_path, &updates)?;
        assert_eq!(
            result,
            "Updated Line 1\nUpdated Line 2\nLine 3\nUpdated Line 4\nUpdated Line 5\n"
        );
        Ok(())
    }

    #[test]
    fn test_create_initial_tree() -> Result<()> {
        let (temp_dir, explorer) = setup_test_directory()?;

        // Create a simple file system structure
        fs::create_dir(temp_dir.path().join("dir1"))?;
        fs::create_dir(temp_dir.path().join("dir2"))?;
        create_test_file(temp_dir.path(), "file1.txt", "content")?;
        create_test_file(&temp_dir.path().join("dir1"), "file2.txt", "content")?;

        let tree = explorer.create_initial_tree(2)?;

        // Assert basic structure
        assert!(tree.is_expanded);
        assert_eq!(tree.entry_type, FileSystemEntryType::Directory);

        // Assert the children
        let children_names: Vec<String> = tree.children.keys().cloned().collect();
        assert!(children_names.contains(&"dir1".to_string()));
        assert!(children_names.contains(&"dir2".to_string()));
        assert!(children_names.contains(&"file1.txt".to_string()));

        // Assert dir1
        let dir1 = tree.children.get("dir1").unwrap();
        assert_eq!(dir1.entry_type, FileSystemEntryType::Directory);
        assert!(dir1.is_expanded);
        assert!(dir1.children.contains_key("file2.txt"));

        Ok(())
    }

    #[test]
    fn test_apply_updates_invalid_line_number() -> Result<()> {
        let (temp_dir, explorer) = setup_test_directory()?;
        let file_path = create_test_file(temp_dir.path(), "test.txt", "content")?;

        let updates = vec![FileUpdate {
            start_line: 0, // Invalid line number
            end_line: 1,
            new_content: "new content".to_string(),
        }];

        let result = explorer.apply_updates(&file_path, &updates);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Line numbers must start at 1"
        );
        Ok(())
    }

    #[test]
    fn test_apply_updates_out_of_bounds() -> Result<()> {
        let (temp_dir, explorer) = setup_test_directory()?;
        let file_path = create_test_file(temp_dir.path(), "test.txt", "single line")?;

        let updates = vec![FileUpdate {
            start_line: 1,
            end_line: 10, // The file only has 1 line
            new_content: "new content".to_string(),
        }];

        let result = explorer.apply_updates(&file_path, &updates);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "End line 10 exceeds file length 1"
        );
        Ok(())
    }
}
