use crate::explorer::Explorer;
use crate::types::{CodeExplorer, Project};
use anyhow::Result;
use confy;
use dirs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

// Set XDG_CONFIG_HOME if you want configs in ~/.config
// This is optional but recommended if you want more accessible config location

// Default config file name
const APP_NAME: &str = "code-assistant";
const CONFIG_NAME: &str = "projects";

/// Project configuration stored on disk
#[derive(Debug, Serialize, Deserialize, Default)]
struct ProjectsConfig {
    projects: HashMap<String, PathBuf>,
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
    // Set XDG_CONFIG_HOME to ~/.config if not already set
    if env::var("XDG_CONFIG_HOME").is_err() {
        if let Some(home) = dirs::home_dir() {
            let config_dir = home.join(".config");
            env::set_var("XDG_CONFIG_HOME", config_dir);
        }
    }

    // With confy 0.6.1, we can't specify JSON format directly
    // Try to load from confy's default location first
    let config: ProjectsConfig = confy::load(APP_NAME, CONFIG_NAME).unwrap_or_default();

    // Convert PathBuf to Project objects
    let projects = config
        .projects
        .into_iter()
        .map(|(name, path)| (name, Project { path }))
        .collect();

    Ok(projects)
}
