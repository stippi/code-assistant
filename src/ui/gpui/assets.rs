use anyhow::anyhow;
use gpui::{AssetSource, Result, SharedString};
use std::{borrow::Cow, fs, path::Path};

/// A simple asset source implementation that loads assets from the filesystem.
///
/// This is a simplified version of Zed's assets system that just loads files
/// directly from the disk rather than embedding them in the binary.
#[derive(Clone)]
pub struct Assets {
    /// Base path to the assets directory
    base_path: String,
}

impl Assets {
    /// Creates a new Assets instance with the given base path.
    pub fn new(base_path: impl Into<String>) -> Self {
        Self {
            base_path: base_path.into(),
        }
    }

    /// Get the absolute path to an asset.
    pub fn get_path(&self, path: &str) -> String {
        format!("{}/{}", self.base_path, path)
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let full_path = self.get_path(path);
        let path = Path::new(&full_path);

        if !path.exists() {
            eprintln!("DEBUG [Assets]: Asset not found: {}", full_path);
            return Ok(None);
        }

        let result = fs::read(path)
            .map(|data| Some(Cow::<'static, [u8]>::Owned(data)))
            .map_err(|e| {
                eprintln!("DEBUG [Assets]: Failed to read asset: {}", e);
                anyhow!("Failed to read asset at {}: {}", full_path, e)
            });

        if let Ok(Some(ref data)) = result {
            eprintln!(
                "DEBUG [Assets]: Successfully loaded asset: {} ({} bytes)",
                full_path,
                data.len()
            );
        }

        result
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let full_path = self.get_path(path);
        let path = Path::new(&full_path);

        if !path.exists() || !path.is_dir() {
            return Ok(Vec::new());
        }

        let entries = std::fs::read_dir(path)
            .map_err(|e| anyhow!("Failed to read directory at {}: {}", full_path, e))?;

        let mut result = Vec::new();
        for entry in entries {
            if let Ok(entry) = entry {
                if let Some(name) = entry.file_name().to_str() {
                    let asset_path = format!("{}/{}", path.display(), name);
                    result.push(SharedString::from(asset_path));
                }
            }
        }

        Ok(result)
    }
}
