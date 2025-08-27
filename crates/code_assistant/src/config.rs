use crate::explorer::Explorer;
use crate::types::{CodeExplorer, Project};
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;

/// Get the path to the configuration file
pub fn get_config_path() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let config_dir = home.join(".config").join("code-assistant");
    std::fs::create_dir_all(&config_dir)?; // Ensure directory exists
    Ok(config_dir.join("projects.json"))
}

// The main trait for project management
pub trait ProjectManager: Send + Sync {
    // Add a temporary project, returns the project name
    fn add_temporary_project(&mut self, path: PathBuf) -> Result<String>;
    // Get all projects (both configured and temporary)
    fn get_projects(&self) -> Result<HashMap<String, Project>>;
    fn get_project(&self, name: &str) -> Result<Option<Project>>;
    fn get_explorer_for_project(&self, name: &str) -> Result<Box<dyn CodeExplorer>>;
}

// Default implementation of ProjectManager that loads from config file
pub struct DefaultProjectManager {
    temp_projects: HashMap<String, Project>,
}

impl DefaultProjectManager {
    pub fn new() -> Self {
        Self {
            temp_projects: HashMap::new(),
        }
    }
}

impl ProjectManager for DefaultProjectManager {
    fn add_temporary_project(&mut self, path: PathBuf) -> Result<String> {
        // Canonicalize path
        let path = path.canonicalize()?;

        // Check if this path matches any existing project
        let projects = load_projects()?;
        for (name, project) in &projects {
            if project.path == path {
                return Ok(name.clone());
            }
        }

        // Generate name from path leaf
        let mut name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("temp_project")
            .to_string();

        // Ensure name is unique
        let mut counter = 1;
        let original_name = name.clone();
        while projects.contains_key(&name) {
            name = format!("{original_name}_{counter}");
            counter += 1;
        }

        // Add to temporary projects
        self.temp_projects.insert(
            name.clone(),
            Project {
                path,
                format_on_save: None,
            },
        );

        Ok(name)
    }

    fn get_projects(&self) -> Result<HashMap<String, Project>> {
        let mut all_projects = load_projects()?;
        all_projects.extend(self.temp_projects.clone());
        Ok(all_projects)
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
