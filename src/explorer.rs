use crate::types::{
    CodeExplorer, FileSystemEntryType, FileTreeEntry, FileUpdate, SearchMode, SearchOptions,
    SearchResult,
};
use anyhow::Result;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
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
                self.expand_directory(entry_path, &mut child_entry, current_depth + 1, max_depth)?;
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

    fn write_file(&self, path: &PathBuf, content: &String) -> Result<()> {
        debug!("Writing file: {}", path.display());
        // Ensure the parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(std::fs::write(path, content)?)
    }

    fn delete_file(&self, path: &PathBuf) -> Result<()> {
        std::fs::remove_file(path)?;
        Ok(())
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
                path.as_path(),
                &mut entry,
                0,
                max_depth.unwrap_or(usize::MAX),
            )?;
        }

        Ok(entry)
    }

    fn apply_updates(&self, path: &Path, updates: &[FileUpdate]) -> Result<String> {
        let content = std::fs::read_to_string(path)?;
        let updated_content = crate::utils::apply_content_updates(&content, updates)?;

        // Update the stored content
        std::fs::write(path, &updated_content)?;

        Ok(updated_content)
    }

    fn search(&self, path: &Path, options: SearchOptions) -> Result<Vec<SearchResult>> {
        let mut results = Vec::new();
        let max_results = options.max_results.unwrap_or(usize::MAX);

        // Prepare regex for different search modes
        let regex = match options.mode {
            SearchMode::Exact => {
                // For exact search, escape regex special characters and optionally add word boundaries
                let pattern = if options.whole_words {
                    format!(r"\b{}\b", regex::escape(&options.query))
                } else {
                    regex::escape(&options.query)
                };
                RegexBuilder::new(&pattern)
                    .case_insensitive(!options.case_sensitive)
                    .build()?
            }
            SearchMode::Regex => {
                // For regex search, optionally add word boundaries to user's pattern
                let pattern = if options.whole_words {
                    format!(r"\b{}\b", options.query)
                } else {
                    options.query.clone()
                };
                RegexBuilder::new(&pattern)
                    .case_insensitive(!options.case_sensitive)
                    .build()?
            }
        };

        let walker = WalkBuilder::new(path)
            .hidden(false)
            .git_ignore(true)
            .build();

        for entry in walker {
            let entry = entry?;
            let path = entry.path();

            // Skip directories and non-text files
            if path.is_dir() || !is_text_file(path) {
                continue;
            }

            let file = std::fs::File::open(path)?;
            let reader = BufReader::new(file);

            for (line_idx, line) in reader.lines().enumerate() {
                let line = line?;

                // Find all matches in the line
                let matches: Vec<_> = regex.find_iter(&line).collect();
                if !matches.is_empty() {
                    results.push(SearchResult {
                        file: path.to_path_buf(),
                        line_number: line_idx + 1,
                        line_content: line.to_string(),
                        match_ranges: matches.iter().map(|m| (m.start(), m.end())).collect(),
                    });

                    if results.len() >= max_results {
                        return Ok(results);
                    }
                }
            }
        }

        Ok(results)
    }
}

/// Helper function to determine if a file is likely to be a text file
fn is_text_file(path: &Path) -> bool {
    let text_extensions = [
        "txt",
        "md",
        "rs",
        "js",
        "py",
        "java",
        "c",
        "cpp",
        "h",
        "hpp",
        "css",
        "html",
        "xml",
        "json",
        "yaml",
        "yml",
        "toml",
        "sh",
        "bash",
        "zsh",
        "fish",
        "conf",
        "cfg",
        "ini",
        "properties",
        "env",
    ];

    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| text_extensions.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
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
            end_line: 4,
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
                end_line: 3,
                new_content: "Updated Line 1\nUpdated Line 2".to_string(),
            },
            FileUpdate {
                start_line: 4,
                end_line: 6,
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
    fn test_search() -> Result<()> {
        let (temp_dir, explorer) = setup_test_directory()?;

        // Create test files with content
        create_test_file(
            temp_dir.path(),
            "file1.txt",
            "This is line 1\nThis is line 2\nThis is line 3",
        )?;
        create_test_file(
            temp_dir.path(),
            "file2.txt",
            "Another file line 1\nAnother file line 2",
        )?;

        // Create a subdirectory with a file
        fs::create_dir(temp_dir.path().join("subdir"))?;
        create_test_file(
            &temp_dir.path().join("subdir"),
            "file3.txt",
            "Subdir line 1\nSubdir line 2",
        )?;

        // Test searching with different queries
        let results = explorer.search(
            temp_dir.path(),
            SearchOptions {
                query: "line 2".to_string(),
                ..Default::default()
            },
        )?;
        assert_eq!(results.len(), 3);
        assert!(results
            .iter()
            .any(|r| r.line_content.contains("This is line 2")));
        assert!(results
            .iter()
            .any(|r| r.line_content.contains("Another file line 2")));
        assert!(results
            .iter()
            .any(|r| r.line_content.contains("Subdir line 2")));

        // Test with max_results
        let results = explorer.search(
            temp_dir.path(),
            SearchOptions {
                query: "line".to_string(),
                max_results: Some(2),
                ..Default::default()
            },
        )?;
        assert_eq!(results.len(), 2);

        // Test with non-matching query
        let results = explorer.search(
            temp_dir.path(),
            SearchOptions {
                query: "nonexistent".to_string(),
                ..Default::default()
            },
        )?;
        assert_eq!(results.len(), 0);

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
}
