use crate::types::Project;
use anyhow::Result;
use fs_explorer::{CodeExplorer, Explorer};
use sandbox::SandboxContext;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Configuration for running the agent in either terminal or GPUI mode
#[derive(Debug, Clone)]
pub struct AgentRunConfig {
    pub path: PathBuf,
    pub task: Option<String>,
    pub continue_task: bool,
    pub model: String,
    pub tool_syntax: crate::types::ToolSyntax,
    pub use_diff_format: bool,
    pub record: Option<PathBuf>,
    pub playback: Option<PathBuf>,
    pub fast_playback: bool,
    pub sandbox_policy: sandbox::SandboxPolicy,
}

/// Get the path to the configuration file
pub fn get_config_path() -> Result<PathBuf> {
    let config_dir = crate::config_dir::config_dir();
    std::fs::create_dir_all(&config_dir)?; // Ensure directory exists
    Ok(config_dir.join("projects.json"))
}

// The main trait for project management
pub trait ProjectManager: Send + Sync {
    // Add a temporary project, returns the project name.
    // Takes &self so managers can be shared (e.g. Arc); implementations use
    // interior mutability for their temporary-project state.
    fn add_temporary_project(&self, path: PathBuf) -> Result<String>;
    // Get all projects (both configured and temporary)
    fn get_projects(&self) -> Result<HashMap<String, Project>>;
    fn get_project(&self, name: &str) -> Result<Option<Project>>;
    fn get_explorer_for_project(&self, name: &str) -> Result<Box<dyn CodeExplorer>>;
}

/// Reserved scope token addressing the user skills directory.
pub const SCOPE_CONFIG: &str = ":config:";
/// Reserved scope token addressing the bundled (system) skills directory.
pub const SCOPE_SYSTEM: &str = ":system:";

/// Resolve a reserved skills-scope token to its sandboxed root directory,
/// relative to `config_dir`. Returns `None` for ordinary project names.
///
/// - [`SCOPE_CONFIG`] (`:config:`) → `<config_dir>/skills` (user skills)
/// - [`SCOPE_SYSTEM`] (`:system:`) → `<config_dir>/skills/.system` (bundled skills)
///
/// The roots are deliberately the `skills` subtree, never `config_dir` itself —
/// the config directory also holds secrets (e.g. `providers.json`), which must
/// stay unreachable through file tools.
pub fn skills_scope_root(scope: &str, config_dir: &Path) -> Option<PathBuf> {
    match scope {
        SCOPE_CONFIG => Some(config_dir.join("skills")),
        SCOPE_SYSTEM => Some(config_dir.join("skills").join(".system")),
        _ => None,
    }
}

/// Resolve a scope reference to a sandboxed explorer.
///
/// A reserved token ([`SCOPE_CONFIG`]/[`SCOPE_SYSTEM`]) resolves to an explorer
/// rooted at the corresponding skills directory; any other value is treated as
/// a project name and delegated to the [`ProjectManager`].
pub fn explorer_for_scope(
    project_manager: &dyn ProjectManager,
    scope: &str,
) -> Result<Box<dyn CodeExplorer>> {
    if let Some(root) = skills_scope_root(scope, &crate::config_dir::config_dir()) {
        return Ok(Box::new(Explorer::new(root)));
    }
    project_manager.get_explorer_for_project(scope)
}

pub struct SandboxAwareProjectManager {
    inner: Box<dyn ProjectManager>,
    sandbox_context: Arc<SandboxContext>,
}

impl SandboxAwareProjectManager {
    pub fn new(inner: Box<dyn ProjectManager>, sandbox_context: Arc<SandboxContext>) -> Self {
        Self {
            inner,
            sandbox_context,
        }
    }

    fn register_project_root(&self, project: &Project) {
        if let Err(err) = self.sandbox_context.register_root(&project.path) {
            tracing::warn!(
                "Failed to register sandbox root {}: {}",
                project.path.display(),
                err
            );
        }
    }
}

impl ProjectManager for SandboxAwareProjectManager {
    fn add_temporary_project(&self, path: PathBuf) -> Result<String> {
        let name = self.inner.add_temporary_project(path.clone())?;
        if let Some(project) = self.inner.get_project(&name)? {
            self.register_project_root(&project);
        } else if let Err(err) = self.sandbox_context.register_root(path) {
            tracing::warn!("Failed to register sandbox root for temp project: {}", err);
        }
        Ok(name)
    }

    fn get_projects(&self) -> Result<HashMap<String, Project>> {
        let projects = self.inner.get_projects()?;
        for project in projects.values() {
            self.register_project_root(project);
        }
        Ok(projects)
    }

    fn get_project(&self, name: &str) -> Result<Option<Project>> {
        let project = self.inner.get_project(name)?;
        if let Some(ref project) = project {
            self.register_project_root(project);
        }
        Ok(project)
    }

    fn get_explorer_for_project(&self, name: &str) -> Result<Box<dyn CodeExplorer>> {
        if let Some(project) = self.inner.get_project(name)? {
            self.register_project_root(&project);
        }
        self.inner.get_explorer_for_project(name)
    }
}

// Default implementation of ProjectManager that loads from config file
#[derive(Default)]
pub struct DefaultProjectManager {
    temp_projects: std::sync::Mutex<HashMap<String, Project>>,
}

impl DefaultProjectManager {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ProjectManager for DefaultProjectManager {
    fn add_temporary_project(&self, path: PathBuf) -> Result<String> {
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
        self.temp_projects.lock().unwrap().insert(
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
        all_projects.extend(self.temp_projects.lock().unwrap().clone());
        Ok(all_projects)
    }

    fn get_project(&self, name: &str) -> Result<Option<Project>> {
        let projects = self.get_projects()?;
        Ok(projects.get(name).cloned())
    }

    fn get_explorer_for_project(&self, name: &str) -> Result<Box<dyn CodeExplorer>> {
        let project = self
            .get_project(name)?
            .ok_or_else(|| anyhow::anyhow!("Project not found: {name}"))?;

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

/// Save a single project to the projects.json configuration file.
/// Creates the file if it doesn't exist yet.
pub fn save_project(name: &str, project: &Project) -> Result<()> {
    let config_path = get_config_path()?;
    let mut projects = load_projects()?;
    projects.insert(name.to_string(), project.clone());

    crate::utils::file_utils::atomic_write_json(&config_path, &projects)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn skills_scope_root_maps_reserved_tokens_only() {
        let cfg = Path::new("/cfg");
        assert_eq!(
            skills_scope_root(SCOPE_CONFIG, cfg),
            Some(PathBuf::from("/cfg/skills"))
        );
        assert_eq!(
            skills_scope_root(SCOPE_SYSTEM, cfg),
            Some(PathBuf::from("/cfg/skills/.system"))
        );
        // Ordinary project names are not scope tokens.
        assert_eq!(skills_scope_root("my-project", cfg), None);
        assert_eq!(skills_scope_root("config", cfg), None);
    }

    #[tokio::test]
    async fn config_scope_cannot_escape_to_config_secrets() {
        // Lay out a config dir with secrets next to the skills subtree.
        let config_dir = tempdir().unwrap();
        std::fs::write(
            config_dir.path().join("providers.json"),
            "{\"api_key\":\"super-secret\"}",
        )
        .unwrap();

        let skills_root = skills_scope_root(SCOPE_CONFIG, config_dir.path()).unwrap();
        std::fs::create_dir_all(&skills_root).unwrap();

        let explorer = Explorer::new(skills_root.clone());

        // A traversal attempt to the sibling secrets file must be rejected.
        let escape = skills_root.join("..").join("providers.json");
        let result = explorer.read_file(&escape).await;
        assert!(
            result.is_err(),
            "config scope must not be able to read outside the skills subtree"
        );
    }
}
