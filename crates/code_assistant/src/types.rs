use clap::ValueEnum;
use command_executor::build_format_command;
use fs_explorer::FileTreeEntry;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use web::{WebPage, WebSearchResult};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Project {
    pub path: PathBuf,
    #[serde(default)]
    pub format_on_save: Option<HashMap<String, String>>,
}

impl Project {
    /// Returns the formatter command template configured for the given relative path, if any.
    /// Iteration over patterns is deterministic (sorted by pattern string).
    pub fn formatter_template_for(&self, rel_path: &Path) -> Option<String> {
        let mapping = self.format_on_save.as_ref()?;
        let file_name = rel_path.to_string_lossy();

        // Sort patterns deterministically to avoid HashMap ordering
        let mut entries: Vec<(&String, &String)> = mapping.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));

        for (pattern, command) in entries {
            if let Ok(glob) = glob::Pattern::new(pattern) {
                if glob.matches(&file_name) {
                    return Some(command.clone());
                }
            } else {
                // Fallback: simple substring match if glob pattern failed to parse
                if file_name.contains(pattern) {
                    return Some(command.clone());
                }
            }
        }
        None
    }

    /// Builds a formatter command for the given relative path using the optional {path} placeholder.
    pub fn format_command_for(&self, rel_path: &Path) -> Option<String> {
        self.formatter_template_for(rel_path)
            .map(|template| build_format_command(&template, rel_path))
    }
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

/// Priority levels for plan items
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanItemPriority {
    High,
    #[default]
    Medium,
    Low,
}

/// Execution status for plan items
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanItemStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

/// A single plan item maintained by the agent
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PlanItem {
    pub content: String,
    #[serde(default)]
    pub priority: PlanItemPriority,
    #[serde(default)]
    pub status: PlanItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "_meta")]
    pub meta: Option<JsonValue>,
}

/// Complete plan state for a session
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct PlanState {
    #[serde(default)]
    pub entries: Vec<PlanItem>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "_meta")]
    pub meta: Option<JsonValue>,
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

/// Tool description for LLM
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Unknown tool: {0}")]
    UnknownTool(String),

    #[error("Failed to parse tool parameters: {0}")]
    ParseError(String),
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
