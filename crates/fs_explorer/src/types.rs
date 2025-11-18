use anyhow::Result;
use command_executor::CommandExecutor;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum FileEncoding {
    #[default]
    UTF8,
    UTF16LE,
    UTF16BE,
    Windows1252,
    ISO8859_2,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum LineEnding {
    #[default]
    LF,
    Crlf,
    CR,
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileTreeEntry {
    pub name: String,
    pub entry_type: FileSystemEntryType,
    pub children: HashMap<String, FileTreeEntry>,
    pub is_expanded: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum FileSystemEntryType {
    File,
    Directory,
}

/// Details for a text replacement operation
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileReplacement {
    pub search: String,
    pub replace: String,
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Debug, Clone, Default)]
pub enum SearchMode {
    #[default]
    Exact,
    Regex,
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
    pub start_line: usize,
    pub line_content: Vec<String>,
    pub match_lines: Vec<usize>,
    pub match_ranges: Vec<Vec<(usize, usize)>>,
}

#[async_trait::async_trait]
pub trait CodeExplorer: Send + Sync {
    fn root_dir(&self) -> PathBuf;
    async fn read_file(&self, path: &Path) -> Result<String>;
    async fn read_file_range(
        &self,
        path: &Path,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> Result<String>;
    async fn write_file(&self, path: &Path, content: &str, append: bool) -> Result<String>;
    async fn delete_file(&self, path: &Path) -> Result<()>;
    fn create_initial_tree(&mut self, max_depth: usize) -> Result<FileTreeEntry>;
    async fn list_files(&mut self, path: &Path, max_depth: Option<usize>) -> Result<FileTreeEntry>;
    async fn apply_replacements(
        &self,
        path: &Path,
        replacements: &[FileReplacement],
    ) -> Result<String>;
    async fn apply_replacements_with_formatting(
        &self,
        path: &Path,
        replacements: &[FileReplacement],
        format_command: &str,
        command_executor: &dyn CommandExecutor,
    ) -> Result<(String, Option<Vec<FileReplacement>>)>;
    async fn search(&self, path: &Path, options: SearchOptions) -> Result<Vec<SearchResult>>;
    fn clone_box(&self) -> Box<dyn CodeExplorer>;
}
