use crate::types::WorkingMemory;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persistent state of the agent
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentState {
    /// Current working directory when the state was saved
    pub working_dir: PathBuf,
    /// Original task
    pub task: String,
    /// Working memory state
    pub memory: WorkingMemory,
}

const STATE_FILE: &str = ".code-assistant.state.json";

impl AgentState {
    /// Create a new state file
    pub fn new(working_dir: PathBuf, task: String, memory: WorkingMemory) -> Self {
        Self {
            working_dir,
            task,
            memory,
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
    pub fn load(working_dir: &PathBuf) -> Result<Option<Self>> {
        let state_path = working_dir.join(STATE_FILE);
        if !state_path.exists() {
            return Ok(None);
        }

        let json = std::fs::read_to_string(state_path)?;
        let state = serde_json::from_str(&json)?;
        Ok(Some(state))
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
