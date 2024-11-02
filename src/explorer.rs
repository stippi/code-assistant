use anyhow::Result;
use std::path::PathBuf;
use tracing::info;
use walkdir::WalkDir;

/// Handles file system operations for code exploration
pub struct CodeExplorer {
    root_dir: PathBuf,
}

impl CodeExplorer {
    /// Creates a new CodeExplorer instance
    ///
    /// # Arguments
    /// * `root_dir` - The root directory to explore
    pub fn new(root_dir: PathBuf) -> Self {
        Self { root_dir }
    }

    /// Lists all files in the repository recursively
    ///
    /// # Returns
    /// * `Result<Vec<PathBuf>>` - List of file paths or an error
    pub fn list_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        for entry in WalkDir::new(&self.root_dir)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                files.push(entry.path().to_owned());
            }
        }

        info!("Found {} files", files.len());
        Ok(files)
    }

    /// Reads the content of a file
    ///
    /// # Arguments
    /// * `path` - Path to the file to read
    ///
    /// # Returns
    /// * `Result<String>` - File content or an error
    pub fn read_file(&self, path: &PathBuf) -> Result<String> {
        Ok(std::fs::read_to_string(path)?)
    }
}
