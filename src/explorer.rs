use crate::types::{
    CodeExplorer, FileReplacement, FileSystemEntryType, FileTreeEntry, SearchMode, SearchOptions,
    SearchResult,
};
use anyhow::Result;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use std::collections::{HashMap, HashSet};
// Removed unused imports
use std::path::{Path, PathBuf};
use tracing::debug;

/// Handles file system operations for code exploration
pub struct Explorer {
    root_dir: PathBuf,
    // Track which paths were explicitly listed
    expanded_paths: HashSet<PathBuf>,
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
        Self {
            root_dir,
            expanded_paths: HashSet::new(),
        }
    }

    fn expand_directory(
        &mut self,
        path: &Path,
        entry: &mut FileTreeEntry,
        current_depth: usize,
        max_depth: usize,
    ) -> Result<()> {
        // Expand if either:
        // - Within max_depth during initial load
        // - The path was explicitly listed before
        let should_expand = current_depth < max_depth || self.expanded_paths.contains(path);

        if !should_expand {
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

    fn create_initial_tree(&mut self, max_depth: usize) -> Result<FileTreeEntry> {
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

        let root_dir = &self.root_dir.clone();
        self.expand_directory(&root_dir, &mut root, 0, max_depth)?;
        Ok(root)
    }

    fn read_file(&self, path: &PathBuf) -> Result<String> {
        debug!("Reading file: {}", path.display());
        Ok(std::fs::read_to_string(path)?)
    }

    fn write_file(&self, path: &PathBuf, content: &String, append: bool) -> Result<()> {
        debug!("Writing file: {}, append: {}", path.display(), append);
        // Ensure the parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if append && path.exists() {
            // Append content to existing file
            let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
            use std::io::Write;
            write!(file, "{}", content)?;
            Ok(())
        } else {
            // Write or overwrite file
            Ok(std::fs::write(path, content)?)
        }
    }

    fn delete_file(&self, path: &PathBuf) -> Result<()> {
        std::fs::remove_file(path)?;
        Ok(())
    }

    fn list_files(&mut self, path: &PathBuf, max_depth: Option<usize>) -> Result<FileTreeEntry> {
        // Remember that this path was explicitly listed
        self.expanded_paths.insert(path.clone());

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

    fn apply_replacements(&self, path: &Path, replacements: &[FileReplacement]) -> Result<String> {
        let content = std::fs::read_to_string(path)?;
        let updated_content = crate::utils::apply_replacements(&content, replacements)?;
        std::fs::write(path, &updated_content)?;
        Ok(updated_content)
    }

    fn search(&self, path: &Path, options: SearchOptions) -> Result<Vec<SearchResult>> {
        let mut results = Vec::new();
        let max_results = options.max_results.unwrap_or(usize::MAX);
        let context_lines = 2; // Lines of context before and after

        // Prepare regex for different search modes
        let regex = match options.mode {
            SearchMode::Exact => {
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

            // Read entire file at once for context lines
            let content = std::fs::read_to_string(path)?;
            let lines: Vec<_> = content.lines().collect();
            let mut current_section: Option<SearchResult> = None;

            for (line_idx, line) in lines.iter().enumerate() {
                let matches: Vec<_> = regex.find_iter(line).collect();

                if !matches.is_empty() {
                    let match_ranges: Vec<_> =
                        matches.iter().map(|m| (m.start(), m.end())).collect();
                    let section_start = line_idx.saturating_sub(context_lines);
                    let section_end = (line_idx + context_lines + 1).min(lines.len());

                    match &mut current_section {
                        // Extend section if close enough to previous match
                        Some(section)
                            if line_idx
                                <= section.start_line
                                    + section.line_content.len()
                                    + context_lines =>
                        {
                            while section.line_content.len() < section_end - section.start_line {
                                section.line_content.push(
                                    lines[section.start_line + section.line_content.len()]
                                        .to_string(),
                                );
                            }
                            section.match_lines.push(line_idx - section.start_line);
                            section.match_ranges.push(match_ranges);
                        }
                        _ => {
                            // Start new section
                            if let Some(section) = current_section.take() {
                                results.push(section);
                                if results.len() >= max_results {
                                    return Ok(results);
                                }
                            }

                            let mut section_lines = Vec::new();
                            for i in section_start..section_end {
                                section_lines.push(lines[i].to_string());
                            }

                            current_section = Some(SearchResult {
                                file: path.to_path_buf(),
                                start_line: section_start,
                                line_content: section_lines,
                                match_lines: vec![line_idx - section_start],
                                match_ranges: vec![match_ranges],
                            });
                        }
                    }
                }
            }

            // Add final section if we have one
            if let Some(section) = current_section {
                results.push(section);
                if results.len() >= max_results {
                    return Ok(results);
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
    fn test_apply_replacements() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let test_file = temp_dir.path().join("test.txt");
        std::fs::write(&test_file, "line 1\nline 2\nline 3")?;

        let explorer = Explorer::new(temp_dir.path().to_path_buf());

        let replacements = vec![
            FileReplacement {
                search: "line 1\n".to_string(),
                replace: "new line 1\n".to_string(),
            },
            FileReplacement {
                search: "line 3".to_string(),
                replace: "new line 3".to_string(),
            },
        ];

        // Apply replacements and verify content
        let result = explorer.apply_replacements(&test_file, &replacements)?;
        assert_eq!(result, "new line 1\nline 2\nnew line 3");

        // Verify file was actually modified
        let content = std::fs::read_to_string(&test_file)?;
        assert_eq!(content, "new line 1\nline 2\nnew line 3");

        // Test error case with ambiguous search
        let result = explorer.apply_replacements(
            &test_file,
            &[FileReplacement {
                search: "line".to_string(),
                replace: "test".to_string(),
            }],
        );
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Found 3 occurrences"));

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
            .any(|r| r.line_content.iter().any(|l| l.contains("This is line 2"))));
        assert!(results.iter().any(|r| r
            .line_content
            .iter()
            .any(|l| l.contains("Another file line 2"))));
        assert!(results
            .iter()
            .any(|r| r.line_content.iter().any(|l| l.contains("Subdir line 2"))));

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
        let (temp_dir, mut explorer) = setup_test_directory()?;

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
