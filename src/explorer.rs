use crate::types::FileSystemEntry;
use crate::types::FileSystemEntryType;
use anyhow::Result;
use std::path::PathBuf;
use tracing::{debug, info};

/// Handles file system operations for code exploration
pub struct CodeExplorer {
    pub root_dir: PathBuf,
}

impl CodeExplorer {
    /// Creates a new CodeExplorer instance
    ///
    /// # Arguments
    /// * `root_dir` - The root directory to explore
    pub fn new(root_dir: PathBuf) -> Self {
        Self { root_dir }
    }

    /// Lists entries in a specific directory (non-recursive)
    pub fn list_directory(&self, dir_path: &PathBuf) -> Result<Vec<FileSystemEntry>> {
        let full_path = if dir_path.is_absolute() {
            dir_path.clone()
        } else {
            self.root_dir.join(dir_path)
        };

        // Ensure the path is within root directory
        if !full_path.starts_with(&self.root_dir) {
            anyhow::bail!("Path is outside of root directory");
        }

        let mut entries = Vec::new();

        for entry in std::fs::read_dir(full_path)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;

            entries.push(FileSystemEntry {
                path: path.clone(),
                name: path
                    .file_name()
                    .ok_or_else(|| anyhow::anyhow!("Invalid filename"))?
                    .to_string_lossy()
                    .into_owned(),
                entry_type: if file_type.is_dir() {
                    FileSystemEntryType::Directory
                } else {
                    FileSystemEntryType::File
                },
            });
        }

        info!("Listing directory: {}", dir_path.display());
        debug!("Found {} entries", entries.len());

        Ok(entries)
    }

    /// Reads the content of a file
    ///
    /// # Arguments
    /// * `path` - Path to the file to read
    ///
    /// # Returns
    /// * `Result<String>` - File content or an error
    pub fn read_file(&self, path: &PathBuf) -> Result<String> {
        info!("Reading file: {}", path.display());
        Ok(std::fs::read_to_string(path)?)
    }
}
