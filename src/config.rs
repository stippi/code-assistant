use crate::types::Project;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;

pub fn get_config_path() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    Ok(home.join(".code-assistant").join("projects.json"))
}

pub fn load_projects() -> Result<HashMap<String, Project>> {
    let config_path = get_config_path()?;
    if !config_path.exists() {
        return Ok(HashMap::new());
    }
    let content = std::fs::read_to_string(config_path)?;
    Ok(serde_json::from_str(&content)?)
}
