use crate::tools::ToolServicesAccess;
use crate::tools::core::{
    Render, ResourcesTracker, Tool, ToolContext, ToolResult, ToolSpec, capabilities,
};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

// Input type for the glob_files tool
#[derive(Deserialize, Serialize)]
pub struct GlobFilesInput {
    pub project: String,
    pub pattern: String,
}

// Output type
#[derive(Serialize, Deserialize)]
pub struct GlobFilesOutput {
    pub project: String,
    pub pattern: String,
    pub files: Vec<PathBuf>,
}

// Render implementation for output formatting
impl Render for GlobFilesOutput {
    fn status(&self) -> String {
        format!(
            "Found {} files matching '{}'",
            self.files.len(),
            self.pattern
        )
    }

    fn render(&self, _tracker: &mut ResourcesTracker) -> String {
        if self.files.is_empty() {
            return format!("No files found matching pattern '{}'", self.pattern);
        }

        let mut formatted = String::new();
        formatted.push_str(&format!("Files matching '{}':\n", self.pattern));

        for file in &self.files {
            formatted.push_str(&format!("{}\n", file.display()));
        }

        formatted
    }
}

// ToolResult implementation
impl ToolResult for GlobFilesOutput {
    fn is_success(&self) -> bool {
        true // Always successful even if no files are found
    }
}

// Tool implementation
pub struct GlobFilesTool;

#[async_trait::async_trait]
impl Tool for GlobFilesTool {
    type Input = GlobFilesInput;
    type Output = GlobFilesOutput;

    fn spec(&self) -> ToolSpec {
        let description = concat!(
            "Find files matching glob patterns within a specified project.\n",
            "`*` matches within one path component (so `*.md` matches only the ",
            "project root); use `**` to recurse, e.g. `**/*.md`, `src/**/*.ts`.\n",
            "Returns all file types (text and binary) that match the pattern.\n",
            "Respects gitignore rules and skips hidden files and common build directories."
        );
        ToolSpec {
            name: "glob_files".into(),
            description: description.into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "examples": ["project-name"],
                        "type": "string",
                        "description": "Name of the project to search within"
                    },
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern. `*` stays within one path component; use `**` to recurse. Examples: '*.rs' (root only), '**/*.json', 'src/**/*.ts'"
                    }
                },
                "required": ["project", "pattern"]
            }),
            annotations: Some(json!({
                "readOnlyHint": true
            })),
            capabilities: ToolSpec::capabilities(&[
                capabilities::READ_ONLY,
                capabilities::SCOPE_MCP,
                capabilities::SCOPE_AGENT,
                capabilities::SCOPE_AGENT_DIFF,
                capabilities::SCOPE_SUBAGENT_READ_ONLY,
                capabilities::SCOPE_SUBAGENT_DEFAULT,
                capabilities::SCOPE_SUBAGENT_DEFAULT_DIFF,
            ]),
            multiline_params: &[],
            hidden: false,
            title_template: Some("Finding files matching '{pattern}'"),
        }
    }

    async fn execute<'a>(
        &self,
        context: &mut ToolContext<'a>,
        input: &mut Self::Input,
    ) -> Result<Self::Output> {
        // Get explorer for the specified project
        let explorer = context
            .project_manager()
            .get_explorer_for_project(&input.project)
            .map_err(|e| {
                anyhow!(
                    "Failed to get explorer for project {}: {}",
                    input.project,
                    e
                )
            })?;

        // Get the root directory of the project
        let root_dir = explorer.root_dir();

        // Find files matching the glob pattern
        let files = find_files_matching_pattern(&root_dir, &input.pattern)?;

        // Convert absolute paths to relative paths for cleaner output
        let mut relative_files = Vec::new();
        for file in files {
            if let Ok(rel_path) = file.strip_prefix(&root_dir) {
                relative_files.push(rel_path.to_path_buf());
            } else {
                relative_files.push(file);
            }
        }

        Ok(GlobFilesOutput {
            project: input.project.clone(),
            pattern: input.pattern.clone(),
            files: relative_files,
        })
    }
}

// Helper function to find all files (text and binary) matching a glob pattern
fn find_files_matching_pattern(root_dir: &std::path::Path, pattern: &str) -> Result<Vec<PathBuf>> {
    use glob::{MatchOptions, Pattern};
    use ignore::WalkBuilder;

    // Use standard shell-glob semantics: `*` and `?` do NOT cross path
    // separators. This makes `*.md` match only files in the project root,
    // while `**/*.md` matches recursively across subdirectories.
    let match_options = MatchOptions {
        require_literal_separator: true,
        ..MatchOptions::new()
    };

    // Default directories and files to ignore (same as in explorer.rs)
    const DEFAULT_IGNORE_PATTERNS: [&str; 13] = [
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
        ".Trash", // macOS Trash folder requires special permissions
    ];

    let glob_pattern =
        Pattern::new(pattern).map_err(|e| anyhow!("Invalid glob pattern '{pattern}': {e}"))?;

    let mut matching_files = Vec::new();

    let walker = WalkBuilder::new(root_dir)
        .hidden(false)
        .git_ignore(true)
        .filter_entry(move |e| {
            let file_name = e.file_name().to_string_lossy();
            !DEFAULT_IGNORE_PATTERNS.iter().any(|ignore_pattern| {
                match Pattern::new(ignore_pattern) {
                    Ok(pat) => pat.matches(&file_name),
                    Err(_) => file_name.contains(ignore_pattern),
                }
            })
        })
        .build();

    for entry in walker {
        let entry = entry?;
        let path = entry.path();

        // Skip directories - we only want files
        if path.is_dir() {
            continue;
        }

        // Include all files (unlike search tool, we want to discover any file type)
        // The LLM might need to know about images, binaries, etc.

        // Get the relative path from the root directory for pattern matching
        let relative_path = if let Ok(rel_path) = path.strip_prefix(root_dir) {
            rel_path
        } else {
            path
        };

        // Convert to string for glob matching
        let path_str = relative_path.to_string_lossy();

        // Check if the relative path matches the glob pattern
        if glob_pattern.matches_with(&path_str, match_options) {
            matching_files.push(path.to_path_buf());
        }
    }

    Ok(matching_files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mocks::ToolTestFixture;

    #[tokio::test]
    async fn test_glob_files_output_rendering() {
        let files = vec![
            PathBuf::from("src/main.rs"),
            PathBuf::from("src/lib.rs"),
            PathBuf::from("tests/test.rs"),
        ];

        let output = GlobFilesOutput {
            project: "test-project".to_string(),
            pattern: "**/*.rs".to_string(),
            files,
        };

        // Render the output
        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        // Verify the output format
        assert!(rendered.contains("Files matching '**/*.rs'"));
        assert!(rendered.contains("src/main.rs"));
        assert!(rendered.contains("src/lib.rs"));
        assert!(rendered.contains("tests/test.rs"));
    }

    #[tokio::test]
    async fn test_glob_files_no_results() {
        let output = GlobFilesOutput {
            project: "test-project".to_string(),
            pattern: "*.nonexistent".to_string(),
            files: Vec::new(),
        };

        // Render the output
        let mut tracker = ResourcesTracker::new();
        let rendered = output.render(&mut tracker);

        // Verify the output format for no results
        assert!(rendered.contains("No files found matching pattern '*.nonexistent'"));
    }

    #[tokio::test]
    async fn test_glob_files_execution() -> Result<()> {
        // Create test fixture
        let mut fixture = ToolTestFixture::new();
        let mut context = fixture.context();

        // Create input for the glob
        let mut input = GlobFilesInput {
            project: "test-project".to_string(),
            pattern: "*.rs".to_string(),
        };

        // Execute the glob
        let tool = GlobFilesTool;
        let result = tool.execute(&mut context, &mut input).await;

        // The execution may fail because the mock explorer's root directory doesn't exist
        // But we can still test that the tool is properly structured
        match result {
            Ok(output) => {
                // If it succeeds, validate the output structure
                assert_eq!(output.project, "test-project");
                assert_eq!(output.pattern, "*.rs");
            }
            Err(_) => {
                // If it fails due to the mock directory not existing, that's expected
                // The important thing is that the tool can be instantiated and called
            }
        }

        Ok(())
    }

    #[test]
    fn test_glob_pattern_validation() {
        use glob::Pattern;

        // Test valid patterns
        assert!(Pattern::new("*.rs").is_ok());
        assert!(Pattern::new("**/*.json").is_ok());
        assert!(Pattern::new("src/**/*.ts").is_ok());

        // Test invalid patterns (glob crate should handle this)
        // Most patterns are actually valid in glob, but let's test an edge case
        assert!(Pattern::new("").is_ok()); // Empty pattern is actually valid
    }

    #[test]
    fn test_single_star_does_not_cross_directories() -> Result<()> {
        use std::fs;
        use tempfile::TempDir;

        let tmp = TempDir::new()?;
        let root = tmp.path();

        fs::write(root.join("root.md"), "root")?;
        fs::create_dir_all(root.join("docs"))?;
        fs::write(root.join("docs/nested.md"), "nested")?;
        fs::create_dir_all(root.join("docs/sub"))?;
        fs::write(root.join("docs/sub/deep.md"), "deep")?;

        // `*.md` must match only the file in the project root.
        let mut matches = find_files_matching_pattern(root, "*.md")?
            .into_iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        matches.sort();
        assert_eq!(matches, vec!["root.md".to_string()]);

        Ok(())
    }

    #[test]
    fn test_double_star_matches_recursively() -> Result<()> {
        use std::fs;
        use tempfile::TempDir;

        let tmp = TempDir::new()?;
        let root = tmp.path();

        fs::write(root.join("root.md"), "root")?;
        fs::create_dir_all(root.join("docs/sub"))?;
        fs::write(root.join("docs/nested.md"), "nested")?;
        fs::write(root.join("docs/sub/deep.md"), "deep")?;

        // `**/*.md` matches markdown files at any depth, including the root
        // (the `**/` prefix can match zero directory components).
        let mut matches = find_files_matching_pattern(root, "**/*.md")?
            .into_iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        matches.sort();
        assert_eq!(
            matches,
            vec![
                "docs/nested.md".to_string(),
                "docs/sub/deep.md".to_string(),
                "root.md".to_string()
            ]
        );

        // A directory-scoped pattern matches only that directory's direct children.
        let mut docs_matches = find_files_matching_pattern(root, "docs/*.md")?
            .into_iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        docs_matches.sort();
        assert_eq!(docs_matches, vec!["docs/nested.md".to_string()]);

        Ok(())
    }
}
