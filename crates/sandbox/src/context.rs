use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Tracks canonical sandbox roots that are allowed for a session.
#[derive(Clone, Default)]
pub struct SandboxContext {
    roots: Arc<RwLock<Vec<PathBuf>>>,
}

impl SandboxContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a root path, canonicalizing it and avoiding duplicates.
    pub fn register_root<P: AsRef<Path>>(&self, path: P) -> io::Result<PathBuf> {
        let canonical = path
            .as_ref()
            .canonicalize()
            .unwrap_or_else(|_| path.as_ref().to_path_buf());

        let mut roots = self.roots.write().expect("sandbox context poisoned");
        if !roots
            .iter()
            .any(|existing| existing.starts_with(&canonical) || canonical.starts_with(existing))
        {
            roots.push(canonical.clone());
        }
        Ok(canonical)
    }

    /// Returns the currently registered roots.
    pub fn roots(&self) -> Vec<PathBuf> {
        self.roots.read().expect("sandbox context poisoned").clone()
    }

    /// Returns whether the candidate path is inside any registered root.
    pub fn is_path_allowed<P: AsRef<Path>>(&self, candidate: P) -> bool {
        let candidate = candidate.as_ref();
        self.roots
            .read()
            .expect("sandbox context poisoned")
            .iter()
            .any(|root| candidate.starts_with(root))
    }
}
