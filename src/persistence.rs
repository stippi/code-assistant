use crate::types::ActionResult;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::debug;

/// Persistent state of the agent
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentState {
    /// Original task description
    pub task: String,
    /// Memory of all previous actions and their results
    pub actions: Vec<ActionResult>,
}

pub trait StatePersistence: Send + Sync {
    fn save_state(&mut self, task: String, actions: Vec<ActionResult>) -> Result<()>;
    fn load_state(&mut self) -> Result<Option<AgentState>>;
    fn cleanup(&mut self) -> Result<()>;
}

pub struct FileStatePersistence {
    root_dir: PathBuf,
}

impl FileStatePersistence {
    pub fn new(root_dir: PathBuf) -> Self {
        Self { root_dir }
    }
}

const STATE_FILE: &str = ".code-assistant.state.json";

impl StatePersistence for FileStatePersistence {
    fn save_state(&mut self, task: String, actions: Vec<ActionResult>) -> Result<()> {
        let state = AgentState { task, actions };
        let state_path = self.root_dir.join(STATE_FILE);
        debug!("Saving state to {}", state_path.display());
        let json = serde_json::to_string_pretty(&state)?;
        std::fs::write(state_path, json)?;
        Ok(())
    }

    fn load_state(&mut self) -> Result<Option<AgentState>> {
        let state_path = self.root_dir.join(STATE_FILE);
        if !state_path.exists() {
            return Ok(None);
        }

        debug!("Loading state from {}", state_path.display());
        let json = std::fs::read_to_string(state_path)?;
        let state = serde_json::from_str(&json)?;
        Ok(Some(state))
    }

    fn cleanup(&mut self) -> Result<()> {
        let state_path = self.root_dir.join(STATE_FILE);
        if state_path.exists() {
            debug!("Removing state file {}", state_path.display());
            std::fs::remove_file(state_path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub struct MockStatePersistence {
    state: Option<AgentState>,
}

#[cfg(test)]
impl MockStatePersistence {
    pub fn new() -> Self {
        Self { state: None }
    }
}

#[cfg(test)]
impl StatePersistence for MockStatePersistence {
    fn save_state(&mut self, task: String, actions: Vec<ActionResult>) -> Result<()> {
        // In-Memory state
        let state = AgentState { task, actions };
        self.state = Some(state);
        Ok(())
    }

    fn load_state(&mut self) -> Result<Option<AgentState>> {
        Ok(self.state.clone())
    }

    fn cleanup(&mut self) -> Result<()> {
        self.state = None;
        Ok(())
    }
}
