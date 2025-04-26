use llm::Message;

use anyhow::Result;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use web::{WebPage, WebSearchResult};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Project {
    pub path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileTreeEntry {
    pub name: String,
    pub entry_type: FileSystemEntryType,
    pub children: HashMap<String, FileTreeEntry>,
    pub is_expanded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LoadedResource {
    File(String), // File content
    WebSearch {
        query: String,
        results: Vec<WebSearchResult>,
    },
    WebPage(WebPage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileEncoding {
    UTF8,
    UTF16LE,
    UTF16BE,
    Windows1252,
    ISO8859_2,
    Other(String),
}

impl Default for FileEncoding {
    fn default() -> Self {
        Self::UTF8
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LineEnding {
    LF,   // Unix: \n
    CRLF, // Windows: \r\n
    CR,   // Legacy Mac: \r
}

impl Default for LineEnding {
    fn default() -> Self {
        Self::LF
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileFormat {
    pub encoding: FileEncoding,
    pub line_ending: LineEnding,
}

impl Default for FileFormat {
    fn default() -> Self {
        Self {
            encoding: FileEncoding::UTF8,
            line_ending: LineEnding::LF,
        }
    }
}

/// Represents the agent's working memory during execution
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct WorkingMemory {
    /// Current task description
    pub current_task: String,
    /// Current plan
    pub plan: String,
    /// Currently loaded resources (files, web search results, web pages)
    /// Key is (project_name, path)
    pub loaded_resources: HashMap<(String, PathBuf), LoadedResource>,
    /// Summaries of previously seen resources
    /// Key is (project_name, path)
    pub summaries: HashMap<(String, PathBuf), String>,
    /// File trees for each project
    pub file_trees: HashMap<String, FileTreeEntry>,
    /// Expanded directories per project
    pub expanded_directories: HashMap<String, Vec<PathBuf>>,
    /// Available project names
    pub available_projects: Vec<String>,
}

impl std::fmt::Display for LoadedResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadedResource::File(content) => write!(f, "{}", content),
            LoadedResource::WebSearch { query, results } => {
                writeln!(f, "Web search results for: '{}'", query)?;
                for result in results {
                    writeln!(f, "- {} ({})", result.title, result.url)?;
                    writeln!(f, "  {}", result.snippet)?;
                }
                Ok(())
            }
            LoadedResource::WebPage(page) => {
                writeln!(f, "Content from: {}", page.url)?;
                write!(f, "{}", page.content)
            }
        }
    }
}

impl WorkingMemory {
    /// Add a new resource to working memory
    pub fn add_resource(&mut self, project: String, path: PathBuf, resource: LoadedResource) {
        self.loaded_resources.insert((project, path), resource);
    }

    /// Update an existing resource if it exists
    pub fn update_resource(
        &mut self,
        project: &str,
        path: &PathBuf,
        resource: LoadedResource,
    ) -> bool {
        let key = (project.to_string(), path.clone());
        if self.loaded_resources.contains_key(&key) {
            self.loaded_resources.insert(key, resource);
            true
        } else {
            false
        }
    }
}

/// Details for a text replacement operation
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileReplacement {
    /// The text to search for. Must match exactly one location in the file
    /// unless replace_all is set to true.
    pub search: String,
    /// The text to replace it with
    pub replace: String,
    /// If true, replaces all occurrences of the search text instead of requiring exactly one match
    #[serde(default)]
    pub replace_all: bool,
}

/// Tool description for LLM
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Unknown tool: {0}")]
    UnknownTool(String),

    #[error("Failed to parse tool parameters: {0}")]
    ParseError(String),
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("LLM error: {0}")]
    LLMError(#[from] anyhow::Error),

    #[error("Action error: {error}")]
    ActionError {
        error: anyhow::Error,
        message: Message,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileSystemEntry {
    pub path: PathBuf,
    pub name: String,
    pub entry_type: FileSystemEntryType,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum FileSystemEntryType {
    File,
    Directory,
}

#[derive(Debug, Clone)]
pub enum SearchMode {
    /// Standard text search, case-insensitive by default
    Exact,
    /// Regular expression search
    Regex,
}

impl Default for SearchMode {
    fn default() -> Self {
        Self::Exact
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub query: String,
    pub case_sensitive: bool,
    pub whole_words: bool,
    pub mode: SearchMode,
    pub max_results: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchResult {
    pub file: PathBuf,
    pub start_line: usize, // First line in the section (including context)
    pub line_content: Vec<String>, // All lines in the section
    pub match_lines: Vec<usize>, // Line numbers with matches (relative to start_line)
    pub match_ranges: Vec<Vec<(usize, usize)>>, // Match positions for each line, aligned with match_lines
}

/// Specifies the tool integration mode
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolMode {
    /// Native tools via API
    Native,
    /// Tools through custom system message with XML tags
    Xml,
}

/// Implements ValueEnum for ToolMode to use with clap
impl ValueEnum for ToolMode {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Native, Self::Xml]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        match self {
            Self::Native => Some(clap::builder::PossibleValue::new("native")),
            Self::Xml => Some(clap::builder::PossibleValue::new("xml")),
        }
    }
}

pub trait CodeExplorer: Send + Sync {
    fn root_dir(&self) -> PathBuf;
    /// Reads the content of a file
    fn read_file(&self, path: &PathBuf) -> Result<String>;
    /// Reads the content of a file between specific line numbers
    fn read_file_range(
        &self,
        path: &PathBuf,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> Result<String>;
    /// Write the content of a file and return the complete content after writing
    fn write_file(&self, path: &PathBuf, content: &String, append: bool) -> Result<String>;
    fn delete_file(&self, path: &PathBuf) -> Result<()>;
    #[allow(dead_code)]
    fn create_initial_tree(&mut self, max_depth: usize) -> Result<FileTreeEntry>;
    fn list_files(&mut self, path: &PathBuf, max_depth: Option<usize>) -> Result<FileTreeEntry>;
    /// Applies FileReplacements to a file
    fn apply_replacements(&self, path: &Path, replacements: &[FileReplacement]) -> Result<String>;
    /// Search for text in files with advanced options
    fn search(&self, path: &Path, options: SearchOptions) -> Result<Vec<SearchResult>>;
}
