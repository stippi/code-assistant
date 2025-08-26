use anyhow::Result;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use web::{WebPage, WebSearchResult};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Project {
    pub path: PathBuf,
    #[serde(default)]
    pub format_on_save: Option<HashMap<String, String>>,
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
    Crlf, // Windows: \r\n
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
    /// Currently loaded resources (files, web search results, web pages)
    /// Key format: "project_name::path" to make it JSON-serializable
    #[serde(with = "tuple_key_map")]
    pub loaded_resources: HashMap<(String, PathBuf), LoadedResource>,
    /// File trees for each project
    pub file_trees: HashMap<String, FileTreeEntry>,
    /// Expanded directories per project
    pub expanded_directories: HashMap<String, Vec<PathBuf>>,
    /// Available project names
    pub available_projects: Vec<String>,
}

/// Custom serialization for HashMap with tuple keys
mod tuple_key_map {
    use super::*;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;

    pub fn serialize<S, V>(
        map: &HashMap<(String, PathBuf), V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        V: Serialize,
    {
        let string_map: HashMap<String, &V> = map
            .iter()
            .map(|((project, path), value)| (format!("{}::{}", project, path.display()), value))
            .collect();
        string_map.serialize(serializer)
    }

    pub fn deserialize<'de, D, V>(
        deserializer: D,
    ) -> Result<HashMap<(String, PathBuf), V>, D::Error>
    where
        D: Deserializer<'de>,
        V: Deserialize<'de>,
    {
        let string_map: HashMap<String, V> = HashMap::deserialize(deserializer)?;
        let result = string_map
            .into_iter()
            .filter_map(|(key, value)| {
                if let Some((project, path_str)) = key.split_once("::") {
                    Some(((project.to_string(), PathBuf::from(path_str)), value))
                } else {
                    None
                }
            })
            .collect();
        Ok(result)
    }
}

impl std::fmt::Display for LoadedResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadedResource::File(content) => write!(f, "{content}"),
            LoadedResource::WebSearch { query, results } => {
                writeln!(f, "Web search results for: '{query}'")?;
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

/// Specifies the tool invocation syntax
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ToolSyntax {
    /// Native tools via API
    Native,
    /// Tools through custom system message with XML tags
    Xml,
    /// Tools through custom system message with triple-caret blocks
    Caret,
}

/// Implements ValueEnum for ToolSyntax to use with clap
impl ValueEnum for ToolSyntax {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Native, Self::Xml, Self::Caret]
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        match self {
            Self::Native => Some(clap::builder::PossibleValue::new("native")),
            Self::Xml => Some(clap::builder::PossibleValue::new("xml")),
            Self::Caret => Some(clap::builder::PossibleValue::new("caret")),
        }
    }
}

pub trait CodeExplorer: Send + Sync {
    fn root_dir(&self) -> PathBuf;
    /// Reads the content of a file
    fn read_file(&self, path: &Path) -> Result<String>;
    /// Reads the content of a file between specific line numbers
    fn read_file_range(
        &self,
        path: &Path,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> Result<String>;
    /// Write the content of a file and return the complete content after writing
    fn write_file(&self, path: &Path, content: &str, append: bool) -> Result<String>;
    fn delete_file(&self, path: &Path) -> Result<()>;
    #[allow(dead_code)]
    fn create_initial_tree(&mut self, max_depth: usize) -> Result<FileTreeEntry>;
    fn list_files(&mut self, path: &Path, max_depth: Option<usize>) -> Result<FileTreeEntry>;
    /// Applies FileReplacements to a file
    fn apply_replacements(&self, path: &Path, replacements: &[FileReplacement]) -> Result<String>;
    /// Search for text in files with advanced options
    fn search(&self, path: &Path, options: SearchOptions) -> Result<Vec<SearchResult>>;
    /// Create a cloned box of this explorer
    #[allow(dead_code)]
    fn clone_box(&self) -> Box<dyn CodeExplorer>;
}
