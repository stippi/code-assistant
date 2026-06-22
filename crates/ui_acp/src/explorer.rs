use agent_client_protocol::schema as acp;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::ClientConn;
use code_assistant_core::config::{DefaultProjectManager, ProjectManager};
use code_assistant_core::types::Project;
use command_executor::CommandExecutor;
use fs_explorer::encoding::{
    apply_file_format, detect_line_ending, detect_trailing_whitespace, normalize_content,
    normalize_line_endings,
};
use fs_explorer::file_updater::{
    apply_matches, apply_replacements_normalized, extract_stable_ranges, find_replacement_matches,
    reconstruct_formatted_replacements,
};
use fs_explorer::{
    is_path_gitignored, CodeExplorer, Explorer, FileEncoding, FileFormat, FileReplacement,
    FileTreeEntry, SearchOptions, SearchResult,
};
use tokio::task::spawn_blocking;

/// Filesystem access proxied through the connected ACP client.
///
/// The SDK connection is `Send + Clone`, so we just hold a
/// `ConnectionTo<Client>` and issue `fs/read_text_file` / `fs/write_text_file`
/// requests directly from the agent task.
async fn fs_read_text_file(
    conn: &ClientConn,
    request: acp::ReadTextFileRequest,
) -> Result<acp::ReadTextFileResponse> {
    conn.send_request(request)
        .block_task()
        .await
        .map_err(|e| anyhow!("Failed to read file via ACP: {e}"))
}

async fn fs_write_text_file(
    conn: &ClientConn,
    request: acp::WriteTextFileRequest,
) -> Result<acp::WriteTextFileResponse> {
    conn.send_request(request)
        .block_task()
        .await
        .map_err(|e| anyhow!("Failed to write file via ACP: {e}"))
}

pub struct AcpCodeExplorer {
    session_id: acp::SessionId,
    conn: ClientConn,
    root_dir: PathBuf,
    delegate: Explorer,
    file_formats: Arc<RwLock<HashMap<PathBuf, FileFormat>>>,
}

impl AcpCodeExplorer {
    pub fn new(root_dir: PathBuf, session_id: acp::SessionId, conn: ClientConn) -> Self {
        Self {
            session_id,
            conn,
            delegate: Explorer::new(root_dir.clone()),
            root_dir,
            file_formats: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn to_absolute(&self, path: &Path) -> Result<PathBuf> {
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root_dir.join(path)
        };

        if abs.starts_with(&self.root_dir) {
            Ok(abs)
        } else {
            Err(anyhow!(
                "Path {} is outside of the project root {}",
                abs.display(),
                self.root_dir.display()
            ))
        }
    }

    fn ensure_allowed(&self, path: &Path) -> Result<PathBuf> {
        let abs = self.to_absolute(path)?;
        if is_path_gitignored(&self.root_dir, &abs) {
            return Err(anyhow!(
                "Access to files ignored by .gitignore not allowed: {}",
                abs.display()
            ));
        }
        Ok(abs)
    }

    fn store_format(&self, path: &Path, format: FileFormat) {
        if let Ok(mut map) = self.file_formats.write() {
            map.insert(path.to_path_buf(), format);
        }
    }

    fn format_for_path(&self, path: &Path) -> FileFormat {
        self.file_formats
            .read()
            .ok()
            .and_then(|m| m.get(path).cloned())
            .unwrap_or_default()
    }

    async fn read_entire(&self, path: &Path) -> Result<(String, PathBuf)> {
        let abs = self.ensure_allowed(path)?;
        let response = fs_read_text_file(
            &self.conn,
            acp::ReadTextFileRequest::new(self.session_id.clone(), abs.clone()),
        )
        .await?;

        let line_ending = detect_line_ending(&response.content);
        let has_trailing_whitespace = detect_trailing_whitespace(&response.content);
        self.store_format(
            &abs,
            FileFormat {
                encoding: FileEncoding::UTF8,
                line_ending,
                has_trailing_whitespace,
            },
        );

        let normalized = if has_trailing_whitespace {
            normalize_line_endings(&response.content)
        } else {
            normalize_content(&response.content)
        };
        Ok((normalized, abs))
    }

    async fn write_entire(&self, path: &Path, content: &str) -> Result<String> {
        let abs = self.ensure_allowed(path)?;
        let format = self.format_for_path(&abs);
        let formatted = apply_file_format(content, &format);
        fs_write_text_file(
            &self.conn,
            acp::WriteTextFileRequest::new(self.session_id.clone(), abs.clone(), formatted),
        )
        .await?;
        let response = fs_read_text_file(
            &self.conn,
            acp::ReadTextFileRequest::new(self.session_id.clone(), abs.clone()),
        )
        .await?;

        let line_ending = detect_line_ending(&response.content);
        let has_trailing_whitespace = detect_trailing_whitespace(&response.content);
        self.store_format(
            &abs,
            FileFormat {
                encoding: FileEncoding::UTF8,
                line_ending,
                has_trailing_whitespace,
            },
        );
        let normalized = if has_trailing_whitespace {
            normalize_line_endings(&response.content)
        } else {
            normalize_content(&response.content)
        };
        Ok(normalized)
    }
}

#[async_trait::async_trait]
impl CodeExplorer for AcpCodeExplorer {
    fn clone_box(&self) -> Box<dyn CodeExplorer> {
        Box::new(AcpCodeExplorer {
            session_id: self.session_id.clone(),
            conn: self.conn.clone(),
            root_dir: self.root_dir.clone(),
            delegate: self.delegate.clone(),
            file_formats: self.file_formats.clone(),
        })
    }

    fn root_dir(&self) -> PathBuf {
        self.root_dir.clone()
    }

    fn create_initial_tree(&mut self, max_depth: usize) -> Result<FileTreeEntry> {
        self.delegate.create_initial_tree(max_depth)
    }

    async fn read_file(&self, path: &Path) -> Result<String> {
        let (content, _) = self.read_entire(path).await?;
        Ok(content)
    }

    async fn read_file_range(
        &self,
        path: &Path,
        start_line: Option<usize>,
        end_line: Option<usize>,
    ) -> Result<String> {
        let content = self.read_file(path).await?;
        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            return Ok(String::new());
        }
        let total_lines = lines.len();
        let start = start_line.unwrap_or(1).saturating_sub(1);
        // Cap end to total_lines - 1 to prevent out-of-bounds access
        let end = end_line
            .map(|e| e.saturating_sub(1).min(total_lines - 1))
            .unwrap_or(total_lines - 1);
        if start >= total_lines || start > end {
            return Err(anyhow!(
                "Invalid line range: start={}, end={}, total_lines={}",
                start + 1,
                end + 1,
                total_lines
            ));
        }
        Ok(lines[start..=end].join("\n"))
    }

    async fn write_file(&self, path: &Path, content: &str, append: bool) -> Result<String> {
        let mut new_content = content.to_string();
        if append {
            if let Ok((existing, _)) = self.read_entire(path).await {
                new_content = format!("{existing}{content}");
            }
        }
        self.write_entire(path, &new_content).await
    }

    async fn delete_file(&self, path: &Path) -> Result<()> {
        let abs = self.ensure_allowed(path)?;
        let abs_for_io = abs.clone();
        spawn_blocking(move || std::fs::remove_file(&abs_for_io)).await??;
        if let Ok(mut map) = self.file_formats.write() {
            map.remove(&abs);
        }
        Ok(())
    }

    async fn list_files(&mut self, path: &Path, max_depth: Option<usize>) -> Result<FileTreeEntry> {
        self.delegate.list_files(path, max_depth).await
    }

    async fn apply_replacements(
        &self,
        path: &Path,
        replacements: &[FileReplacement],
    ) -> Result<String> {
        let (original_content, _) = self.read_entire(path).await?;
        let format = self.format_for_path(path);
        let updated = apply_replacements_normalized(
            &original_content,
            replacements,
            format.has_trailing_whitespace,
        )?;
        self.write_entire(path, &updated).await
    }

    async fn apply_replacements_with_formatting(
        &self,
        path: &Path,
        replacements: &[FileReplacement],
        _format_command: &str,
        _command_executor: &dyn CommandExecutor,
    ) -> Result<(String, Option<Vec<FileReplacement>>)> {
        let (original_content, _) = self.read_entire(path).await?;
        let format = self.format_for_path(path);
        let preserve_trailing_ws = format.has_trailing_whitespace;
        let (matches, has_conflicts) =
            find_replacement_matches(&original_content, replacements, preserve_trailing_ws)?;
        let updated_content = apply_matches(
            &original_content,
            &matches,
            replacements,
            preserve_trailing_ws,
        )?;
        let final_content = self.write_entire(path, &updated_content).await?;
        let updated_replacements = if has_conflicts {
            None
        } else {
            let stable_ranges =
                extract_stable_ranges(&original_content, &matches, preserve_trailing_ws);
            reconstruct_formatted_replacements(
                &original_content,
                &final_content,
                &stable_ranges,
                &matches,
                replacements,
            )
        };
        Ok((final_content, updated_replacements))
    }

    async fn search(&self, path: &Path, options: SearchOptions) -> Result<Vec<SearchResult>> {
        self.delegate.search(path, options).await
    }
}

pub struct AcpProjectManager {
    inner: DefaultProjectManager,
    session_id: acp::SessionId,
    conn: ClientConn,
    /// The root directory of the ACP session (i.e., the project opened in Zed).
    /// Only projects with a path matching this root will use the ACP explorer.
    acp_root: Option<PathBuf>,
}

impl AcpProjectManager {
    pub fn new(
        inner: DefaultProjectManager,
        session_id: acp::SessionId,
        conn: ClientConn,
        acp_root: Option<PathBuf>,
    ) -> Self {
        let acp_root = acp_root.and_then(|p| p.canonicalize().ok());
        Self {
            inner,
            session_id,
            conn,
            acp_root,
        }
    }

    fn maybe_project(&self, name: &str) -> Result<Project> {
        self.inner
            .get_project(name)?
            .ok_or_else(|| anyhow!("Project not found: {name}"))
    }

    /// Returns true if the given project path matches the ACP session root.
    fn is_acp_project(&self, project_path: &Path) -> bool {
        let Some(acp_root) = &self.acp_root else {
            return false;
        };
        let canonical = project_path.canonicalize().ok();
        canonical.as_ref() == Some(acp_root)
    }
}

impl ProjectManager for AcpProjectManager {
    fn add_temporary_project(&self, path: PathBuf) -> Result<String> {
        self.inner.add_temporary_project(path)
    }

    fn get_projects(&self) -> Result<HashMap<String, Project>> {
        self.inner.get_projects()
    }

    fn get_project(&self, name: &str) -> Result<Option<Project>> {
        self.inner.get_project(name)
    }

    fn get_explorer_for_project(&self, name: &str) -> Result<Box<dyn CodeExplorer>> {
        let project = self.maybe_project(name)?;

        // Use ACP explorer only for the project that matches the ACP session root.
        // Other projects (e.g., referenced via settings) use the standard local explorer
        // because Zed's ACP filesystem access is restricted to the current project.
        if self.is_acp_project(&project.path) {
            Ok(Box::new(AcpCodeExplorer::new(
                project.path,
                self.session_id.clone(),
                self.conn.clone(),
            )))
        } else {
            tracing::debug!(
                "Using local explorer for project '{}' at {} (outside ACP root)",
                name,
                project.path.display()
            );
            Ok(Box::new(Explorer::new(project.path)))
        }
    }
}
