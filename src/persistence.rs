use crate::types::{ActionResult, CodeExplorer, FileTreeEntry, WorkingMemory};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Reduced version of WorkingMemory for persistence
#[derive(Debug, Serialize, Deserialize)]
pub struct PersistentMemory {
    /// Only names of loaded files (content will be reloaded)
    pub loaded_files: Vec<PathBuf>,
    /// Summaries of previously seen files
    pub file_summaries: Vec<(PathBuf, String)>,
    /// Complete file tree of the repository
    pub file_tree: Option<FileTreeEntry>,
    /// Current task description
    pub current_task: String,
    /// Memory of previous actions and their results
    pub action_history: Vec<ActionResult>,
    /// Additional context or notes the agent has generated
    pub notes: Vec<String>,
}

impl From<WorkingMemory> for PersistentMemory {
    fn from(memory: WorkingMemory) -> Self {
        Self {
            loaded_files: memory.loaded_files.keys().cloned().collect(),
            file_summaries: memory.file_summaries.into_iter().collect(),
            file_tree: memory.file_tree,
            current_task: memory.current_task,
            action_history: memory.action_history,
            notes: memory.notes,
        }
    }
}

impl PersistentMemory {
    fn into_working_memory(self, explorer: &dyn CodeExplorer) -> Result<WorkingMemory> {
        let mut loaded_files = std::collections::HashMap::new();
        for path in self.loaded_files {
            // If file can't be read, just skip it - it might have been deleted
            if let Ok(content) = explorer.read_file(&path) {
                loaded_files.insert(path, content);
            }
        }

        Ok(WorkingMemory {
            loaded_files,
            file_summaries: self.file_summaries.into_iter().collect(),
            file_tree: self.file_tree,
            current_task: self.current_task,
            action_history: self.action_history,
            notes: self.notes,
        })
    }
}

/// Persistent state of the agent
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentState {
    /// Current working directory when the state was saved
    pub working_dir: PathBuf,
    /// Original task
    pub task: String,
    /// Working memory state
    pub memory: PersistentMemory,
}

const STATE_FILE: &str = ".code-assistant.state.json";

impl AgentState {
    /// Create a new state file
    pub fn new(working_dir: PathBuf, task: String, memory: WorkingMemory) -> Self {
        Self {
            working_dir,
            task,
            memory: memory.into(),
        }
    }

    /// Save state to disk
    pub fn save(&self) -> Result<()> {
        let state_path = self.working_dir.join(STATE_FILE);
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(state_path, json)?;
        Ok(())
    }

    /// Load state from disk if it exists
    pub fn load(
        working_dir: &PathBuf,
        explorer: &dyn CodeExplorer,
    ) -> Result<Option<(Self, WorkingMemory)>> {
        let state_path = working_dir.join(STATE_FILE);
        if !state_path.exists() {
            return Ok(None);
        }

        let json = std::fs::read_to_string(state_path)?;
        let state: AgentState = serde_json::from_str(&json)?;
        let memory = state.memory.into_working_memory(explorer)?;
        Ok(Some((state, memory)))
    }

    /// Remove state file
    pub fn cleanup(working_dir: &PathBuf) -> Result<()> {
        let state_path = working_dir.join(STATE_FILE);
        if state_path.exists() {
            std::fs::remove_file(state_path)?;
        }
        Ok(())
    }
}
