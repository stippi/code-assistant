use crate::types::ActionResult;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::debug;

/// Minimal persistent state for the agent
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentState {
    /// Original task description
    pub task: String,
    /// Memory of all actions and their results
    pub actions: Vec<ActionResult>,
}

const STATE_FILE: &str = ".code-assistant.state.json";

impl AgentState {
    /// Create a new state file
    pub fn new(task: String, actions: Vec<ActionResult>) -> Self {
        Self { task, actions }
    }

    /// Save state to disk
    pub fn save(&self, working_dir: &PathBuf) -> Result<()> {
        let state_path = working_dir.join(STATE_FILE);
        debug!("Saving state to {}", state_path.display());
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

        debug!("Loading state from {}", state_path.display());
        let json = std::fs::read_to_string(state_path)?;
        let state = serde_json::from_str(&json)?;
        Ok(Some(state))
    }

    /// Remove state file
    pub fn cleanup(working_dir: &PathBuf) -> Result<()> {
        let state_path = working_dir.join(STATE_FILE);
        if state_path.exists() {
            debug!("Removing state file {}", state_path.display());
            std::fs::remove_file(state_path)?;
        }
        Ok(())
    }
}
