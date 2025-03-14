use anyhow::anyhow;
use gpui::{AssetSource, Result, SharedString};
use std::{borrow::Cow, fs::File, io::Read, path::Path};

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
            return Ok(None);
        }

        let mut file = File::open(path)
            .map_err(|e| anyhow!("Failed to open asset at {}: {}", full_path, e))?;

        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)
            .map_err(|e| anyhow!("Failed to read asset at {}: {}", full_path, e))?;

        Ok(Some(Cow::Owned(buffer)))
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

// Singleton instance for global access
static mut ASSETS_INSTANCE: Option<Assets> = None;

/// Initialize the assets system with the given base path.
pub fn init(base_path: impl Into<String>) {
    unsafe {
        ASSETS_INSTANCE = Some(Assets::new(base_path));
    }
}

/// Get the assets instance.
///
/// # Panics
///
/// This function will panic if the assets system has not been initialized.
pub fn get() -> &'static Assets {
    unsafe { ASSETS_INSTANCE.as_ref().expect("Assets not initialized") }
}
