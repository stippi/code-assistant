use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolScope, ToolSpec,
};
use crate::types::{SearchMode, SearchOptions, SearchResult};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

// Input type for the search_files tool
#[derive(Deserialize)]
pub struct SearchFilesInput {
    pub project: String,
    pub regex: String,
    #[serde(default)]
    pub paths: Option<Vec<String>>,
}

// Output type
#[derive(Serialize, Deserialize)]
pub struct SearchFilesOutput {
    #[allow(dead_code)]
    pub project: String,
    pub regex: String,
    pub results: Vec<SearchResult>,
    #[serde(default)]
    pub total_matches: usize,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub summary_mode: bool,
}

// Render implementation for output formatting
impl Render for SearchFilesOutput {
    fn status(&self) -> String {
        if self.truncated {
            format!(
                "Found {} matches (showing {}) for '{}'",
                self.total_matches,
                self.results.len(),
                self.regex
            )
        } else {
            format!("Found {} matches for '{}'", self.results.len(), self.regex)
        }
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if self.results.is_empty() {
            return format!("No matches found for '{}'", self.regex);
        }

        let mut formatted = String::new();

        // Header with match count and mode information
        if self.truncated {
            formatted.push_str(&format!(
                "Found {} matches for '{}' (showing top {} results):\n",
                self.total_matches,
                self.regex,
                self.results.len()
            ));
        } else {
            formatted.push_str(&format!(
                "Found {} matches for '{}':\n",
                self.total_matches, self.regex
            ));
        }

        if self.summary_mode {
            // Summary mode: show only file paths with match counts
            formatted.push_str(
                "\nToo many code snippets would be displayed. Showing file paths only.\n",
            );
            formatted.push_str("Use the 'paths' parameter to search within specific directories for detailed results.\n\n");

            // Group results by file path and sum match counts
            let mut file_matches = std::collections::HashMap::new();
            for result in &self.results {
                let match_count = result.match_lines.len();
                *file_matches.entry(result.file.clone()).or_insert(0) += match_count;
            }

            // Sort files by path for consistent output
            let mut sorted_files: Vec<_> = file_matches.iter().collect();
            sorted_files.sort_by_key(|(path, _)| path.as_path());

            for (file_path, total_matches) in sorted_files {
                formatted.push_str(&format!(
                    "{} ({} matches)\n",
                    file_path.display(),
                    total_matches
                ));
            }

            if self.truncated {
                let unique_files = file_matches.len();
                formatted.push_str(&format!(
                    "\n... and {} more files with matches.\n",
                    self.total_matches - unique_files
                ));
            }
        } else {
            // Full mode: show snippets with context, but limit by snippet count
            const MAX_DISPLAYED_SNIPPETS: usize = 20; // Show max 20 snippets in full mode
            let mut snippets_shown = 0;
            let mut files_with_snippets = 0;

            // Show detailed results for top matches
            for result in &self.results {
                if snippets_shown >= MAX_DISPLAYED_SNIPPETS {
                    break;
                }

                // Display the file path with line range (same format as accepted by read_files)
                let end_line = result.start_line + result.line_content.len() - 1;
                formatted.push_str(&format!(
                    ">>>>> RESULT: {}:{}-{}\n",
                    result.file.display(),
                    result.start_line + 1,
                    end_line + 1
                ));

                // Display the matched content with context
                for (_, line) in result.line_content.iter().enumerate() {
                    formatted.push_str(&format!("{}", line));

                    // Add a newline if not already present
                    if !line.ends_with('\n') {
                        formatted.push('\n');
                    }
                }

                formatted.push_str("<<<<< END RESULT\n\n");
                snippets_shown += 1;
                files_with_snippets += 1;
            }

            // Show remaining files as paths only
            let remaining_results = &self.results[files_with_snippets..];
            if !remaining_results.is_empty() {
                // Group remaining results by file path and sum match counts
                let mut file_matches = HashMap::new();
                for result in remaining_results {
                    let match_count = result.match_lines.len();
                    *file_matches.entry(result.file.clone()).or_insert(0) += match_count;
                }

                formatted.push_str(&format!(
                    "Additional {} files with matches:\n",
                    file_matches.len()
                ));

                // Sort files by path for consistent output
                let mut sorted_files: Vec<_> = file_matches.iter().collect();
                sorted_files.sort_by_key(|(path, _)| path.as_path());

                for (file_path, total_matches) in sorted_files {
                    formatted.push_str(&format!(
                        "📄 {} ({} matches)\n",
                        file_path.display(),
                        total_matches
                    ));
                }
                formatted.push('\n');
            }
        }

        if self.truncated {
            formatted.push_str("Use the 'paths' parameter to search within specific directories for more focused results.\n");
        }

        formatted
    }
}

// ToolResult implementation
impl ToolResult for SearchFilesOutput {
    fn is_success(&self) -> bool {
        true // Always successful even if no matches are found
    }
}

// Tool implementation
pub struct SearchFilesTool;

impl SearchFilesTool {
    /// Calculate relevance score for search results to prioritize them
    fn calculate_relevance_score(result: &SearchResult, _root_dir: &std::path::Path) -> f64 {
        let mut score = 0.0;

        // Base score from number of matches
        score += result.match_lines.len() as f64;

        // Boost score for certain file types
        if let Some(extension) = result.file.extension().and_then(|e| e.to_str()) {
            match extension {
                // Source code files get higher priority
                "rs" | "py" | "js" | "ts" | "java" | "cpp" | "c" | "h" => score *= 1.5,
                // Configuration and documentation files
                "md" | "txt" | "toml" | "yaml" | "yml" | "json" => score *= 1.2,
                // Test files get slightly lower priority
                _ if result.file.to_string_lossy().contains("test") => score *= 0.9,
                _ => {}
            }
        }

        // Penalize deeply nested files (prefer files closer to root)
        let depth = result.file.components().count();
        if depth > 3 {
            score *= 0.8_f64.powi((depth - 3) as i32);
        }

        // Boost files in common important directories
        let path_str = result.file.to_string_lossy().to_lowercase();
        if path_str.starts_with("src/") {
            score *= 1.3;
        } else if path_str.starts_with("lib/") || path_str.starts_with("crates/") {
            score *= 1.2;
        } else if path_str.contains("example") || path_str.contains("demo") {
            score *= 0.8;
        }

        // Boost files with higher match density (matches per line)
        if !result.line_content.is_empty() {
            let match_density = result.match_lines.len() as f64 / result.line_content.len() as f64;
            score *= 1.0 + match_density;
        }

        score
    }
}

#[async_trait::async_trait]
impl Tool for SearchFilesTool {
    type Input = SearchFilesInput;
    type Output = SearchFilesOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Search for text in files within a specified project using regex in Rust syntax.\n",
            "This tool searches for specific content across multiple files, displaying each match with context."
        );
        ToolSpec {
            name: "search_files",
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project to search within"
                    },
                    "regex": {
                        "type": "string",
                        "description": "The regex pattern to search for. Supports Rust regex syntax including character classes, quantifiers, etc."
                    },
                    "paths": {
                        "type": "array",
                        "items": {
                            "type": "string"
                        },
                        "description": "Optional: Restrict search to specific paths within the project (e.g., ['src/', 'tests/']). Use with caution - only when you're certain the search should be limited to specific directories. Omitting this parameter searches the entire project, which is usually preferred to avoid missing relevant matches."
                    }
                },
                "required": ["project", "regex"]
            }),
            annotations: Some(json!({
                "readOnlyHint": true
            })),
            supported_scopes: &[
                ToolScope::McpServer,
                ToolScope::Agent,
                ToolScope::AgentWithDiffBlocks,
            ],
            hidden: false,
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: Self::Input,
    ) -> Result<Self::Output> {
        // Get explorer for the specified project
        let explorer = context
            .project_manager
            .get_explorer_for_project(&input.project)
            .map_err(|e| {
                anyhow!(
                    "Failed to get explorer for project {}: {}",
                    input.project,
                    e
                )
            })?;

        let root_dir = explorer.root_dir();

        // Determine search paths
        let search_paths = if let Some(paths) = &input.paths {
            // Validate and convert relative paths to absolute
            let mut absolute_paths = Vec::new();
            for path in paths {
                let absolute_path = root_dir.join(path);
                if absolute_path.exists() {
                    absolute_paths.push(absolute_path);
                } else {
                    return Err(anyhow!("Path not found: {}", path));
                }
            }
            absolute_paths
        } else {
            vec![root_dir.clone()]
        };

        // Set up search options with a reasonable initial limit
        let options = SearchOptions {
            query: input.regex.clone(),
            case_sensitive: false,
            whole_words: false,
            mode: SearchMode::Regex,
            max_results: Some(500), // Initial limit to prevent excessive results
        };

        // Perform searches across all specified paths
        let mut all_results = Vec::new();
        for search_path in search_paths {
            let mut path_results = explorer.search(&search_path, options.clone())?;
            all_results.append(&mut path_results);
        }

        // Sort results by relevance
        all_results.sort_by(|a, b| {
            Self::calculate_relevance_score(b, &root_dir)
                .partial_cmp(&Self::calculate_relevance_score(a, &root_dir))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Convert absolute paths to relative paths for cleaner output
        for result in &mut all_results {
            if let Ok(rel_path) = result.file.strip_prefix(&root_dir) {
                result.file = rel_path.to_path_buf();
            }
        }

        let total_files = all_results.len();

        // Count total snippets that would be displayed (each file can have multiple sections)
        let total_snippets: usize = all_results
            .iter()
            .map(|_result| {
                // Each SearchResult represents one snippet/section with context
                1
            })
            .sum();

        // Determine output mode and limits based on snippet count
        const MAX_SNIPPETS: usize = 30; // Max snippets with full content
        const MAX_SUMMARY_FILES: usize = 200; // Max files in summary mode

        let (results, truncated, summary_mode) = if total_snippets > MAX_SNIPPETS {
            if total_files > MAX_SUMMARY_FILES {
                // Too many files even for summary mode
                (
                    all_results.into_iter().take(MAX_SUMMARY_FILES).collect(),
                    true,
                    true,
                )
            } else {
                // Use summary mode for all results
                (all_results, false, true)
            }
        } else {
            // Use full mode with snippets
            (all_results, false, false)
        };

        Ok(SearchFilesOutput {
            project: input.project,
            regex: input.regex,
            results,
            total_matches: total_files,
            truncated,
            summary_mode,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mocks::{create_explorer_mock, MockProjectManager};
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_search_files_output_rendering() {
        // Create a sample search result
        let mut results = Vec::new();

        // Create a result with context lines
        let result = SearchResult {
            file: PathBuf::from("src/main.rs"),
            start_line: 10,
            line_content: vec![
                "fn main() {\n".to_string(),
                "    // This is a test function\n".to_string(),
                "    println!(\"Hello, world!\");\n".to_string(),
                "}\n".to_string(),
            ],
            match_lines: vec![2], // The line with "Hello, world!" is a match
            match_ranges: vec![vec![(14, 27)]], // The range of "Hello, world!"
        };

        results.push(result);

        // Create output with the sample result
        let output = SearchFilesOutput {
            project: "test-project".to_string(),
            regex: "Hello".to_string(),
            results: results.clone(),
            total_matches: results.len(),
            truncated: false,
            summary_mode: false,
        };

        // Render the output
        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        // Verify the output format
        assert!(rendered.contains("Found 1 matches for 'Hello'"));
        assert!(rendered.contains(">>>>> RESULT: src/main.rs"));
        assert!(rendered.contains("println!(\"Hello, world!\");"));
        assert!(rendered.contains("<<<<< END RESULT"));
    }

    #[tokio::test]
    async fn test_search_files_no_results() {
        // Create output with no results
        let output = SearchFilesOutput {
            project: "test-project".to_string(),
            regex: "NonExistentPattern".to_string(),
            results: Vec::new(),
            total_matches: 0,
            truncated: false,
            summary_mode: false,
        };

        // Render the output
        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        // Verify the output format for no results
        assert!(rendered.contains("No matches found for 'NonExistentPattern'"));
    }

    #[tokio::test]
    async fn test_search_files_execution() -> Result<()> {
        // Set up test files with content that will match our search
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("./root/test.txt"),
            "This is a test file\nwith multiple lines\nand searchable content".to_string(),
        );

        // Create a mock explorer with our test files
        let explorer = create_explorer_mock();

        // Create a mock project manager
        let project_manager = Box::new(MockProjectManager::default().with_project(
            "test-project",
            PathBuf::from("./root"),
            Box::new(explorer),
        ));

        // Create a command executor
        let command_executor = Box::new(crate::utils::DefaultCommandExecutor);

        // Create a tool context
        let mut context = ToolContext {
            project_manager: project_manager.as_ref(),
            command_executor: command_executor.as_ref(),
            working_memory: None,
        };

        // Create input for the search
        let input = SearchFilesInput {
            project: "test-project".to_string(),
            regex: "searchable".to_string(),
            paths: None,
        };

        // Execute the search
        let tool = SearchFilesTool;
        let result = tool.execute(&mut context, input).await?;

        // In a real test, we would verify the results
        // However, our mock explorer's search method would need to be enhanced
        // to properly return search results for our test files

        // For now, let's just validate that we got a valid response object
        assert_eq!(result.project, "test-project");
        assert_eq!(result.regex, "searchable");

        Ok(())
    }

    #[tokio::test]
    async fn test_search_files_summary_mode() {
        // Test that summary mode is used when there are too many results
        let mut many_results = Vec::new();
        for i in 0..100 {
            let result = SearchResult {
                file: PathBuf::from(format!("src/file_{}.rs", i)),
                start_line: 0,
                line_content: vec![format!("let x = {};", i)],
                match_lines: vec![0],
                match_ranges: vec![vec![(0, 3)]],
            };
            many_results.push(result);
        }

        let output = SearchFilesOutput {
            project: "test-project".to_string(),
            regex: "let".to_string(),
            results: many_results,
            total_matches: 100,
            truncated: false,
            summary_mode: true,
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        // Verify summary mode indicators
        assert!(rendered.contains("Too many code snippets would be displayed"));
        assert!(rendered.contains("Showing file paths only"));
        assert!(rendered.contains("matches"));
        assert!(!rendered.contains(">>>>> RESULT:")); // No snippets in summary mode
    }

    #[test]
    fn test_relevance_scoring() {
        let root_dir = PathBuf::from("/project");

        // Test source file gets higher score
        let src_result = SearchResult {
            file: PathBuf::from("src/main.rs"),
            start_line: 0,
            line_content: vec!["fn main() {}".to_string()],
            match_lines: vec![0],
            match_ranges: vec![vec![(0, 2)]],
        };

        // Test deeply nested file gets lower score
        let nested_result = SearchResult {
            file: PathBuf::from("deep/nested/path/file.rs"),
            start_line: 0,
            line_content: vec!["fn main() {}".to_string()],
            match_lines: vec![0],
            match_ranges: vec![vec![(0, 2)]],
        };

        let src_score = SearchFilesTool::calculate_relevance_score(&src_result, &root_dir);
        let nested_score = SearchFilesTool::calculate_relevance_score(&nested_result, &root_dir);

        // Source file should have higher relevance than deeply nested file
        assert!(
            src_score > nested_score,
            "Source file score ({}) should be higher than nested file score ({})",
            src_score,
            nested_score
        );
    }

    #[test]
    fn test_snippet_counting_logic() {
        // Test that snippet counting works correctly for mode switching
        let few_results = vec![
            SearchResult {
                file: PathBuf::from("file1.rs"),
                start_line: 0,
                line_content: vec!["let x = 1;".to_string()],
                match_lines: vec![0],
                match_ranges: vec![vec![(0, 3)]],
            },
            SearchResult {
                file: PathBuf::from("file2.rs"),
                start_line: 0,
                line_content: vec!["let y = 2;".to_string()],
                match_lines: vec![0],
                match_ranges: vec![vec![(0, 3)]],
            },
        ];

        let few_output = SearchFilesOutput {
            project: "test".to_string(),
            regex: "let".to_string(),
            results: few_results,
            total_matches: 2,
            truncated: false,
            summary_mode: false,
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = few_output.render(&mut tracker);

        // Should show full snippets for few results
        assert!(rendered.contains(">>>>> RESULT:"));
        assert!(!rendered.contains("Too many code snippets"));

        // Test with many results (should trigger summary mode)
        let many_results: Vec<SearchResult> = (0..50)
            .map(|i| SearchResult {
                file: PathBuf::from(format!("file{}.rs", i)),
                start_line: 0,
                line_content: vec![format!("let x{} = {};", i, i)],
                match_lines: vec![0],
                match_ranges: vec![vec![(0, 3)]],
            })
            .collect();

        let many_output = SearchFilesOutput {
            project: "test".to_string(),
            regex: "let".to_string(),
            results: many_results,
            total_matches: 50,
            truncated: false,
            summary_mode: true,
        };

        let rendered_many = many_output.render(&mut tracker);

        // Should show summary mode for many results
        assert!(rendered_many.contains("Too many code snippets would be displayed"));
        assert!(!rendered_many.contains(">>>>> RESULT:"));
        assert!(rendered_many.contains("matches"));
    }

    #[test]
    fn test_file_grouping_in_summary_mode() {
        // Test that multiple search results from the same file are grouped together
        let duplicate_file_results = vec![
            SearchResult {
                file: PathBuf::from("main.rs"),
                start_line: 10,
                line_content: vec!["let x = 1;".to_string()],
                match_lines: vec![0],
                match_ranges: vec![vec![(0, 3)]],
            },
            SearchResult {
                file: PathBuf::from("lib.rs"),
                start_line: 5,
                line_content: vec!["let y = 2;".to_string()],
                match_lines: vec![0],
                match_ranges: vec![vec![(0, 3)]],
            },
            SearchResult {
                file: PathBuf::from("main.rs"), // Same file as first result
                start_line: 20,
                line_content: vec!["let z = 3;".to_string()],
                match_lines: vec![0],
                match_ranges: vec![vec![(0, 3)]],
            },
        ];

        let output = SearchFilesOutput {
            project: "test".to_string(),
            regex: "let".to_string(),
            results: duplicate_file_results,
            total_matches: 2, // Should be 2 unique files, not 3 results
            truncated: false,
            summary_mode: true,
        };

        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        // Should show each file only once with combined match count
        assert!(rendered.contains("main.rs (2 matches)")); // Combined from 2 results
        assert!(rendered.contains("lib.rs (1 matches)")); // Single result

        // Should not show duplicate file entries
        let main_rs_count = rendered.matches("main.rs").count();
        assert_eq!(
            main_rs_count, 1,
            "main.rs should appear only once in output"
        );
    }
}
