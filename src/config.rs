use crate::explorer::Explorer;
use crate::types::{CodeExplorer, Project};
use anyhow::Result;
use dirs;
use serde_json;
use std::collections::HashMap;
use std::path::PathBuf;

/// Get the path to the configuration file
pub fn get_config_path() -> Result<PathBuf> {
    // Use ~/.config instead of ~/.code-assistant
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let config_dir = home.join(".config").join("code-assistant");
    std::fs::create_dir_all(&config_dir)?; // Ensure directory exists
    Ok(config_dir.join("projects.json"))
}

// The main trait for project management
pub trait ProjectManager: Send + Sync {
    fn get_projects(&self) -> Result<HashMap<String, Project>>;
    fn get_project(&self, name: &str) -> Result<Option<Project>>;
    fn get_explorer_for_project(&self, name: &str) -> Result<Box<dyn CodeExplorer>>;
}

// Default implementation of ProjectManager that loads from config file
pub struct DefaultProjectManager;

impl ProjectManager for DefaultProjectManager {
    fn get_projects(&self) -> Result<HashMap<String, Project>> {
        load_projects()
    }

    fn get_project(&self, name: &str) -> Result<Option<Project>> {
        let projects = self.get_projects()?;
        Ok(projects.get(name).cloned())
    }

    fn get_explorer_for_project(&self, name: &str) -> Result<Box<dyn CodeExplorer>> {
        let project = self
            .get_project(name)?
            .ok_or_else(|| anyhow::anyhow!("Project not found: {}", name))?;

        Ok(Box::new(Explorer::new(project.path)))
    }
}

/// Load projects configuration from disk
pub fn load_projects() -> Result<HashMap<String, Project>> {
    let config_path = get_config_path()?;

    if !config_path.exists() {
        return Ok(HashMap::new());
    }

    let content = std::fs::read_to_string(config_path)?;
    Ok(serde_json::from_str(&content)?)
}
