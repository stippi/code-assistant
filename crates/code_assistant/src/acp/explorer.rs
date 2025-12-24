use agent_client_protocol::{self as acp, Client};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use crate::config::{DefaultProjectManager, ProjectManager};
use crate::types::Project;
use command_executor::CommandExecutor;
use fs_explorer::encoding::{apply_file_format, detect_line_ending, normalize_content};
use fs_explorer::file_updater::{
    apply_matches, apply_replacements_normalized, extract_stable_ranges, find_replacement_matches,
    reconstruct_formatted_replacements,
};
use fs_explorer::{
    is_path_gitignored, CodeExplorer, Explorer, FileEncoding, FileFormat, FileReplacement,
    FileTreeEntry, SearchOptions, SearchResult,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::spawn_blocking;

static FS_WORKER: OnceLock<mpsc::UnboundedSender<FsWorkerRequest>> = OnceLock::new();

enum FsWorkerRequest {
    Read {
        request: acp::ReadTextFileRequest,
        reply_tx: oneshot::Sender<Result<acp::ReadTextFileResponse>>,
    },
    Write {
        request: acp::WriteTextFileRequest,
        reply_tx: oneshot::Sender<Result<acp::WriteTextFileResponse>>,
    },
}

fn fs_worker_sender() -> Option<mpsc::UnboundedSender<FsWorkerRequest>> {
    FS_WORKER.get().cloned()
}

async fn fs_read_text_file(request: acp::ReadTextFileRequest) -> Result<acp::ReadTextFileResponse> {
    let sender = fs_worker_sender().ok_or_else(|| anyhow!("ACP FS worker not registered"))?;
    let (tx, rx) = oneshot::channel();
    sender
        .send(FsWorkerRequest::Read {
            request,
            reply_tx: tx,
        })
        .map_err(|_| anyhow!("ACP FS worker unavailable"))?;
    rx.await.map_err(|_| anyhow!("ACP FS worker dropped"))?
}

async fn fs_write_text_file(
    request: acp::WriteTextFileRequest,
) -> Result<acp::WriteTextFileResponse> {
    let sender = fs_worker_sender().ok_or_else(|| anyhow!("ACP FS worker not registered"))?;
    let (tx, rx) = oneshot::channel();
    sender
        .send(FsWorkerRequest::Write {
            request,
            reply_tx: tx,
        })
        .map_err(|_| anyhow!("ACP FS worker unavailable"))?;
    rx.await.map_err(|_| anyhow!("ACP FS worker dropped"))?
}

pub fn register_fs_worker(connection: Arc<acp::AgentSideConnection>) {
    if FS_WORKER.get().is_some() {
        tracing::warn!("ACP FS worker already registered");
        return;
    }

    let (tx, mut rx) = mpsc::unbounded_channel();
    if FS_WORKER.set(tx).is_err() {
        tracing::warn!("ACP FS worker registration raced");
        return;
    }

    tokio::task::spawn_local(async move {
        while let Some(message) = rx.recv().await {
            match message {
                FsWorkerRequest::Read { request, reply_tx } => {
                    let result = connection
                        .read_text_file(request)
                        .await
                        .map_err(|e| anyhow!("Failed to read file via ACP: {e}"));
                    let _ = reply_tx.send(result);
                }
                FsWorkerRequest::Write { request, reply_tx } => {
                    let result = connection
                        .write_text_file(request)
                        .await
                        .map_err(|e| anyhow!("Failed to write file via ACP: {e}"));
                    let _ = reply_tx.send(result);
                }
            }
        }
    });
}

pub struct AcpCodeExplorer {
    session_id: acp::SessionId,
    root_dir: PathBuf,
    delegate: Explorer,
    file_formats: Arc<RwLock<HashMap<PathBuf, FileFormat>>>,
}

impl AcpCodeExplorer {
    pub fn new(root_dir: PathBuf, session_id: acp::SessionId) -> Self {
        Self {
            session_id,
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
        let response = fs_read_text_file(acp::ReadTextFileRequest {
            session_id: self.session_id.clone(),
            path: abs.clone(),
            line: None,
            limit: None,
            meta: None,
        })
        .await?;

        let line_ending = detect_line_ending(&response.content);
        self.store_format(
            &abs,
            FileFormat {
                encoding: FileEncoding::UTF8,
                line_ending,
            },
        );

        Ok((normalize_content(&response.content), abs))
    }

    async fn write_entire(&self, path: &Path, content: &str) -> Result<String> {
        let abs = self.ensure_allowed(path)?;
        let format = self.format_for_path(&abs);
        let formatted = apply_file_format(content, &format);
        fs_write_text_file(acp::WriteTextFileRequest {
            session_id: self.session_id.clone(),
            path: abs.clone(),
            content: formatted,
            meta: None,
        })
        .await?;
        let response = fs_read_text_file(acp::ReadTextFileRequest {
            session_id: self.session_id.clone(),
            path: abs.clone(),
            line: None,
            limit: None,
            meta: None,
        })
        .await?;
        let line_ending = detect_line_ending(&response.content);
        self.store_format(
            &abs,
            FileFormat {
                encoding: FileEncoding::UTF8,
                line_ending,
            },
        );
        Ok(normalize_content(&response.content))
    }
}

#[async_trait::async_trait]
impl CodeExplorer for AcpCodeExplorer {
    fn clone_box(&self) -> Box<dyn CodeExplorer> {
        Box::new(AcpCodeExplorer {
            session_id: self.session_id.clone(),
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
        let updated = apply_replacements_normalized(&original_content, replacements)?;
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
        let (matches, has_conflicts) = find_replacement_matches(&original_content, replacements)?;
        let updated_content = apply_matches(&original_content, &matches, replacements)?;
        let final_content = self.write_entire(path, &updated_content).await?;
        let updated_replacements = if has_conflicts {
            None
        } else {
            let stable_ranges = extract_stable_ranges(&original_content, &matches);
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
    /// The root directory of the ACP session (i.e., the project opened in Zed).
    /// Only projects with a path matching this root will use the ACP explorer.
    acp_root: Option<PathBuf>,
}

impl AcpProjectManager {
    pub fn new(
        inner: DefaultProjectManager,
        session_id: acp::SessionId,
        acp_root: Option<PathBuf>,
    ) -> Self {
        let acp_root = acp_root.and_then(|p| p.canonicalize().ok());
        Self {
            inner,
            session_id,
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
    fn add_temporary_project(&mut self, path: PathBuf) -> Result<String> {
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
