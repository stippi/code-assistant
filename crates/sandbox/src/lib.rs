use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum SandboxPolicy {
    DangerFullAccess,
    ReadOnly,
    WorkspaceWrite {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        writable_roots: Vec<PathBuf>,
        #[serde(default)]
        network_access: bool,
        #[serde(default)]
        exclude_tmpdir_env_var: bool,
        #[serde(default)]
        exclude_slash_tmp: bool,
    },
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        SandboxPolicy::DangerFullAccess
    }
}

impl SandboxPolicy {
    pub fn new_read_only() -> Self {
        Self::ReadOnly
    }

    pub fn new_workspace_write() -> Self {
        Self::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: false,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        }
    }

    pub fn has_full_disk_write_access(&self) -> bool {
        matches!(self, SandboxPolicy::DangerFullAccess)
    }

    pub fn has_full_network_access(&self) -> bool {
        matches!(self, SandboxPolicy::DangerFullAccess)
            || matches!(
                self,
                SandboxPolicy::WorkspaceWrite {
                    network_access: true,
                    ..
                }
            )
    }

    pub fn get_writable_roots_with_cwd(&self, cwd: &Path) -> Vec<WritableRoot> {
        match self {
            SandboxPolicy::DangerFullAccess | SandboxPolicy::ReadOnly => Vec::new(),
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                exclude_tmpdir_env_var,
                exclude_slash_tmp,
                ..
            } => {
                let mut roots = writable_roots.clone();
                roots.push(cwd.to_path_buf());

                if cfg!(unix) && !exclude_slash_tmp {
                    let slash_tmp = PathBuf::from("/tmp");
                    if slash_tmp.is_dir() {
                        roots.push(slash_tmp);
                    }
                }

                if !exclude_tmpdir_env_var {
                    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
                        if !tmpdir.is_empty() {
                            roots.push(PathBuf::from(tmpdir));
                        }
                    }
                }

                roots
                    .into_iter()
                    .map(|root| WritableRoot {
                        root: root.clone(),
                        read_only_subpaths: default_read_only_subpaths(&root),
                    })
                    .collect()
            }
        }
    }

    pub fn requires_restrictions(&self) -> bool {
        !matches!(self, SandboxPolicy::DangerFullAccess)
    }
}

fn default_read_only_subpaths(root: &Path) -> Vec<PathBuf> {
    let mut subpaths = Vec::new();
    let git_dir = root.join(".git");
    if git_dir.is_dir() {
        subpaths.push(git_dir);
    }
    subpaths
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritableRoot {
    pub root: PathBuf,
    pub read_only_subpaths: Vec<PathBuf>,
}

impl WritableRoot {
    pub fn is_path_writable(&self, candidate: &Path) -> bool {
        if !candidate.starts_with(&self.root) {
            return false;
        }

        for sub in &self.read_only_subpaths {
            if candidate.starts_with(sub) {
                return false;
            }
        }

        true
    }
}

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("Sandbox violation: {0}")]
    Violation(String),
    #[error("Sandbox unavailable: {0}")]
    Unavailable(String),
}
#[cfg(target_os = "macos")]
mod seatbelt;
#[cfg(target_os = "macos")]
pub use seatbelt::{SeatbeltInvocation, build_invocation as build_seatbelt_invocation};
