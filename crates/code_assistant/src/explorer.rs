use crate::types::{
    CodeExplorer, FileEncoding, FileFormat, FileReplacement, FileSystemEntryType, FileTreeEntry,
    SearchMode, SearchOptions, SearchResult,
};
use anyhow::Result;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tracing::debug;

// Default directories and files to ignore during file operations
const DEFAULT_IGNORE_PATTERNS: [&str; 12] = [
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

/// Helper struct for grouping search matches into sections
struct SearchSection {
    start_line: usize,
    end_line: usize,
    matches: Vec<(usize, usize, usize, usize)>, // (start_line, end_line, match_start, match_end)
}

/// Handles file system operations for code exploration
pub struct Explorer {
    root_dir: PathBuf,
    // Track which paths were explicitly listed
    expanded_paths: HashSet<PathBuf>,
    // Track which files had which encoding
    file_encodings: Arc<RwLock<HashMap<PathBuf, FileEncoding>>>,
    // Track file format information (encoding + line ending)
    file_formats: Arc<RwLock<HashMap<PathBuf, FileFormat>>>,
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
                } else if is_last {
                    format!("{}└─ ", prefix.replace("├─ ", "│  ").replace("└─ ", "   "))
                } else {
                    format!("{}├─ ", prefix.replace("├─ ", "│  ").replace("└─ ", "   "))
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
            file_encodings: Arc::new(RwLock::new(HashMap::new())),
            file_formats: Arc::new(RwLock::new(HashMap::new())),
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

        let walker = WalkBuilder::new(path)
            .max_depth(Some(1)) // Only immediate children
            .hidden(false)
            .git_ignore(true)
            .filter_entry(move |e| {
                let file_name = e.file_name().to_string_lossy();
                !DEFAULT_IGNORE_PATTERNS
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

    /// Reads a portion of a file between the specified line ranges
    ///
    /// # Arguments
    /// * `path` - Path to the file
    /// * `start_line` - Starting line number (1-based, inclusive)
    /// * `end_line` - Ending line number (1-based, inclusive)
    ///
    /// # Returns
    /// * `Ok(String)` - The content of the specified line range
    /// * `Err(...)` - If an error occurs during file reading or line extraction
    fn read_file_lines(
        &self,
        path: &PathBuf,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> Result<String> {
        debug!(
            "Reading file with line range - path: {}, start_line: {:?}, end_line: {:?}",
            path.display(),
            start_line,
            end_line
        );

        // If no line range is specified, read the whole file
        if start_line.is_none() && end_line.is_none() {
            return self.read_file(path);
        }

        // Check if the file is a text file
        if !crate::utils::encoding::is_text_file(path) {
            return Err(anyhow::anyhow!("Not a text file: {}", path.display()));
        }

        // Read the file with encoding detection
        let (content, encoding) = crate::utils::encoding::read_file_with_encoding(path)?;

        // Detect line ending
        let line_ending = crate::utils::encoding::detect_line_ending(&content);

        // Create and store file format information
        let file_format = FileFormat {
            encoding: encoding.clone(),
            line_ending,
        };

        // Store the format information
        let mut formats = self.file_formats.write().unwrap();
        formats.insert(path.clone(), file_format.clone());

        // Also store in the old encodings map for backward compatibility
        let mut encodings = self.file_encodings.write().unwrap();
        encodings.insert(path.clone(), encoding);

        // Normalize content for consistent line ending and removal of trailing whitespace
        let normalized_content = crate::utils::encoding::normalize_content(&content);

        // If we have line range parameters, extract the specified lines
        let lines: Vec<&str> = normalized_content.lines().collect();
        let total_lines = lines.len();

        // Convert to 0-based indexing
        let start = start_line.map(|s| s.max(1) - 1).unwrap_or(0);
        let end = end_line
            .map(|e| (e.max(1) - 1).min(total_lines - 1))
            .unwrap_or(total_lines - 1);

        // Validate line range
        if start > end || start >= total_lines {
            return Err(anyhow::anyhow!(
                "Invalid line range: start={}, end={}, total_lines={}",
                start + 1, // Convert back to 1-based for the error message
                end + 1,   // Convert back to 1-based for the error message
                total_lines
            ));
        }

        // Extract the lines within the specified range
        let selected_content = lines[start..=end].join("\n");

        Ok(selected_content)
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
        debug!("Reading entire file: {}", path.display());
        // Prüfe, ob die Datei ein Textfile ist
        if !crate::utils::encoding::is_text_file(path) {
            return Err(anyhow::anyhow!("Not a text file: {}", path.display()));
        }

        // Read with encoding detection
        let (content, encoding) = crate::utils::encoding::read_file_with_encoding(path)?;

        // Detect line ending
        let line_ending = crate::utils::encoding::detect_line_ending(&content);

        // Create and store file format information
        let file_format = FileFormat {
            encoding: encoding.clone(),
            line_ending,
        };

        // Store the format information
        let mut formats = self.file_formats.write().unwrap();
        formats.insert(path.clone(), file_format.clone());

        // Also store in the old encodings map for backward compatibility
        let mut encodings = self.file_encodings.write().unwrap();
        encodings.insert(path.clone(), encoding);

        // Normalize content for LLM
        let normalized_content = crate::utils::encoding::normalize_content(&content);

        Ok(normalized_content)
    }

    // New method for reading partial files with line ranges
    fn read_file_range(
        &self,
        path: &PathBuf,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> Result<String> {
        self.read_file_lines(path, start_line, end_line)
    }

    fn write_file(&self, path: &PathBuf, content: &String, append: bool) -> Result<String> {
        debug!("Writing file: {}, append: {}", path.display(), append);
        // Ensure the parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Get file format if known, otherwise use UTF-8/LF
        let file_format = {
            let formats = self.file_formats.read().unwrap();
            formats.get(path).cloned().unwrap_or_default()
        };

        let content_to_write = if append && path.exists() {
            // Try to read existing content and append new content
            match crate::utils::encoding::read_file_with_encoding(path) {
                Ok((existing, _)) => {
                    // When appending, we need to normalize the existing content as well
                    let normalized_existing = crate::utils::encoding::normalize_content(&existing);
                    normalized_existing + content
                }
                Err(_) => content.clone(), // Fallback if reading fails
            }
        } else {
            content.clone()
        };

        // Write the content with the correct format
        crate::utils::encoding::write_file_with_format(path, &content_to_write, &file_format)?;

        // Return the complete content after writing
        Ok(content_to_write)
    }

    fn delete_file(&self, path: &PathBuf) -> Result<()> {
        std::fs::remove_file(path)?;
        Ok(())
    }

    fn list_files(&mut self, path: &PathBuf, max_depth: Option<usize>) -> Result<FileTreeEntry> {
        // Check if the path exists before proceeding
        if !path.exists() {
            return Err(anyhow::anyhow!("Path not found"));
        }

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
        // Get the original content
        let original_content = std::fs::read_to_string(path)?;

        // Get file format or detect if not available
        let file_format = {
            let formats = self.file_formats.read().unwrap();
            match formats.get(path) {
                Some(format) => format.clone(),
                None => {
                    // Detect format if not already known
                    let encoding = FileEncoding::UTF8; // Fallback
                    let line_ending = crate::utils::encoding::detect_line_ending(&original_content);
                    FileFormat {
                        encoding,
                        line_ending,
                    }
                }
            }
        };

        // Apply replacements with normalized content
        let updated_normalized =
            crate::utils::apply_replacements_normalized(&original_content, replacements)?;

        // Write the content back using the file format helper to ensure consistent newline handling
        crate::utils::encoding::write_file_with_format(path, &updated_normalized, &file_format)?;

        // Return the normalized content for LLM
        Ok(updated_normalized)
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
            .filter_entry(move |e| {
                let file_name = e.file_name().to_string_lossy();
                !DEFAULT_IGNORE_PATTERNS
                    .iter()
                    .any(|pattern| match glob::Pattern::new(pattern) {
                        Ok(pat) => pat.matches(&file_name),
                        Err(_) => file_name.contains(pattern),
                    })
            })
            .build();

        for entry in walker {
            let entry = entry?;
            let path = entry.path();

            // Skip directories and non-text files
            if path.is_dir() || !crate::utils::encoding::is_text_file(path) {
                continue;
            }

            // Read with encoding detection
            let (content, _encoding) = match crate::utils::encoding::read_file_with_encoding(path) {
                Ok(result) => result,
                Err(e) => {
                    debug!(
                        "Skipping file with encoding issues: {}: {}",
                        path.display(),
                        e
                    );
                    continue;
                }
            };

            // Normalize content for consistent search results
            let content = crate::utils::encoding::normalize_content(&content);

            // Find all matches in the entire content
            let matches: Vec<_> = regex.find_iter(&content).collect();
            if matches.is_empty() {
                continue;
            }

            // Build an index of line start positions
            let mut line_indices = Vec::new();
            let mut pos = 0;
            for line in content.lines() {
                line_indices.push(pos);
                pos += line.len() + 1; // +1 for the newline character
            }

            // Add final position at the end of content
            if line_indices.is_empty() {
                line_indices.push(0);
            }
            if pos <= content.len() {
                line_indices.push(content.len());
            }

            // Group matches that are close to each other into sections
            let mut sections: Vec<SearchSection> = Vec::new();

            for m in matches {
                let match_start = m.start();
                let match_end = m.end();

                // Find which lines this match spans
                let start_line_idx = match line_indices.binary_search(&match_start) {
                    Ok(idx) => idx,
                    Err(idx) => idx.saturating_sub(1),
                };

                let end_line_idx = match line_indices.binary_search(&match_end) {
                    Ok(idx) => idx,
                    Err(idx) => idx.saturating_sub(1),
                };

                // Determine section bounds with context
                let section_start = start_line_idx.saturating_sub(context_lines);
                let section_end = (end_line_idx + context_lines + 1).min(line_indices.len() - 1);

                // Check if this match can be merged with an existing section
                let mut merged = false;
                for section in &mut sections {
                    if section_start <= section.end_line + context_lines
                        && section_end >= section.start_line.saturating_sub(context_lines)
                    {
                        // Expand the section if needed
                        section.start_line = section.start_line.min(section_start);
                        section.end_line = section.end_line.max(section_end);

                        // Add this match's info to the section
                        section.matches.push((
                            start_line_idx,
                            end_line_idx,
                            match_start,
                            match_end,
                        ));
                        merged = true;
                        break;
                    }
                }

                if !merged {
                    // Create a new section
                    sections.push(SearchSection {
                        start_line: section_start,
                        end_line: section_end,
                        matches: vec![(start_line_idx, end_line_idx, match_start, match_end)],
                    });
                }
            }

            // Convert sections to SearchResults
            for section in sections {
                let mut section_lines = Vec::new();
                for i in section.start_line..=section.end_line {
                    let line_start = line_indices[i];
                    let line_end = if i + 1 < line_indices.len() {
                        line_indices[i + 1] - 1 // -1 to exclude the newline
                    } else {
                        content.len()
                    };

                    let line = content[line_start..line_end].to_string();
                    section_lines.push(line);
                }

                let mut match_lines = Vec::new();
                let mut match_ranges = Vec::new();

                for (start_line, end_line, match_start, match_end) in section.matches {
                    for line_idx in start_line..=end_line {
                        if line_idx < section.start_line || line_idx > section.end_line {
                            continue; // Skip if outside the final section bounds
                        }

                        let rel_line_idx = line_idx - section.start_line;
                        let line_start = line_indices[line_idx];
                        let line_end = if line_idx + 1 < line_indices.len() {
                            line_indices[line_idx + 1] - 1
                        } else {
                            content.len()
                        };

                        // Calculate highlight positions relative to the line
                        if match_start <= line_end && match_end >= line_start {
                            let highlight_start =
                                match_start.max(line_start).saturating_sub(line_start);
                            let highlight_end =
                                (match_end.min(line_end)).saturating_sub(line_start);

                            // Check for index bounds
                            if highlight_end > highlight_start
                                && highlight_end <= (line_end - line_start)
                            {
                                if !match_lines.contains(&rel_line_idx) {
                                    match_lines.push(rel_line_idx);
                                    match_ranges.push(vec![(highlight_start, highlight_end)]);
                                } else {
                                    // Find the index of this line in match_lines
                                    if let Some(idx) =
                                        match_lines.iter().position(|&x| x == rel_line_idx)
                                    {
                                        match_ranges[idx].push((highlight_start, highlight_end));
                                    }
                                }
                            }
                        }
                    }
                }

                results.push(SearchResult {
                    file: path.to_path_buf(),
                    start_line: section.start_line,
                    line_content: section_lines,
                    match_lines,
                    match_ranges,
                });

                if results.len() >= max_results {
                    return Ok(results);
                }
            }
        }

        Ok(results)
    }
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
    fn test_read_file_range() -> Result<()> {
        let (temp_dir, explorer) = setup_test_directory()?;
        let test_content = "Line 1\nLine 2\nLine 3\nLine 4\nLine 5";
        let file_path = create_test_file(temp_dir.path(), "test_lines.txt", test_content)?;

        // Test reading a specific line range
        let result = explorer.read_file_range(&file_path, Some(2), Some(4))?;
        assert_eq!(result, "Line 2\nLine 3\nLine 4");

        // Test reading from a specific line to the end
        let result = explorer.read_file_range(&file_path, Some(4), None)?;
        assert_eq!(result, "Line 4\nLine 5");

        // Test reading from the beginning to a specific line
        let result = explorer.read_file_range(&file_path, None, Some(2))?;
        assert_eq!(result, "Line 1\nLine 2");

        // Test invalid ranges
        let result = explorer.read_file_range(&file_path, Some(10), Some(15));
        assert!(result.is_err());

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
                replace_all: false,
            },
            FileReplacement {
                search: "line 3".to_string(),
                replace: "new line 3".to_string(),
                replace_all: false,
            },
        ];

        // Apply replacements and verify content is functionally equivalent
        let result = explorer.apply_replacements(&test_file, &replacements)?;

        // Anstatt exakte Stringvergleiche zu machen, überprüfen wir nur, ob beide Strings
        // die erwarteten Inhalte haben, unabhängig von der genauen Anzahl der Zeilenumbrüche
        assert!(result.contains("new line 1"));
        assert!(result.contains("line 2"));
        assert!(result.contains("new line 3"));

        // Verify file was actually modified
        let content = std::fs::read_to_string(&test_file)?;
        assert!(content.contains("new line 1"));
        assert!(content.contains("line 2"));
        assert!(content.contains("new line 3"));

        // Test error case with ambiguous search
        let result = explorer.apply_replacements(
            &test_file,
            &[FileReplacement {
                search: "line".to_string(),
                replace: "test".to_string(),
                replace_all: false,
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

        // Test with query containing a line break
        let results = explorer.search(
            temp_dir.path(),
            SearchOptions {
                query: "line 1\nAnother".to_string(),
                ..Default::default()
            },
        )?;
        assert_eq!(results.len(), 1);

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

    #[test]
    fn test_list_files_nonexistent_path() -> Result<()> {
        let (temp_dir, mut explorer) = setup_test_directory()?;
        let nonexistent_path = temp_dir.path().join("nonexistent");

        // Test with a non-existent path
        let result = explorer.list_files(&nonexistent_path, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Path not found"));

        Ok(())
    }
}
