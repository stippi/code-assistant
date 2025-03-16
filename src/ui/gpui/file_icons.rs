use gpui::{App, AssetSource, SharedString};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tracing::{debug, trace, warn};

use crate::ui::gpui::path_util::PathExt;

/// Represents icon information for different file types
#[derive(Deserialize, Debug)]
struct TypeConfig {
    icon: SharedString,
}

/// Configuration for file type associations
#[derive(Deserialize, Debug, Default)]
struct FileTypesConfig {
    stems: HashMap<String, String>,
    suffixes: HashMap<String, String>,
    types: HashMap<String, TypeConfig>,
}

/// A provider for file icons that supports SVG references
pub struct FileIcons {
    /// The loaded configuration from file_types.json
    config: FileTypesConfig,
    /// Fallback emoji icons for when SVGs aren't available
    fallback_stems: HashMap<String, String>,
    fallback_suffixes: HashMap<String, String>,
}

const COLLAPSED_DIRECTORY_TYPE: &str = "collapsed_folder";
const EXPANDED_DIRECTORY_TYPE: &str = "expanded_folder";
const COLLAPSED_CHEVRON_TYPE: &str = "collapsed_chevron";
const EXPANDED_CHEVRON_TYPE: &str = "expanded_chevron";
const WORKING_MEMORY_TYPE: &str = "brain";
const DEFAULT_TYPE: &str = "default";
const FILE_TYPES_ASSET: &str = "icons/file_icons/file_types.json";

impl FileIcons {
    /// Create a new FileIcons instance using the given AssetSource
    pub fn new(assets: Arc<dyn AssetSource>) -> Self {
        // Load the configuration from the JSON file
        let config = Self::load_config(&assets);

        // Initialize fallback emoji mappings
        let mut fallback_stems = HashMap::new();
        let mut fallback_suffixes = HashMap::new();

        // Initialize with common file types as fallbacks
        fallback_suffixes.insert("rs".to_string(), "🦀".to_string());
        fallback_suffixes.insert("js".to_string(), "📜".to_string());
        fallback_suffixes.insert("jsx".to_string(), "⚛️".to_string());
        fallback_suffixes.insert("ts".to_string(), "📘".to_string());
        fallback_suffixes.insert("tsx".to_string(), "⚛️".to_string());
        fallback_suffixes.insert("py".to_string(), "🐍".to_string());
        fallback_suffixes.insert("html".to_string(), "🌐".to_string());
        fallback_suffixes.insert("css".to_string(), "🎨".to_string());
        fallback_suffixes.insert("json".to_string(), "📋".to_string());
        fallback_suffixes.insert("md".to_string(), "📝".to_string());

        // Special file stems
        fallback_stems.insert("Cargo.toml".to_string(), "📦".to_string());
        fallback_stems.insert("package.json".to_string(), "📦".to_string());
        fallback_stems.insert("Dockerfile".to_string(), "🐳".to_string());
        fallback_stems.insert("README.md".to_string(), "📚".to_string());

        Self {
            config,
            fallback_stems,
            fallback_suffixes,
        }
    }

    /// Load configuration from file_types.json
    fn load_config(assets: &Arc<dyn AssetSource>) -> FileTypesConfig {
        debug!("[FileIcons]: Loading config from: {}", FILE_TYPES_ASSET);
        let result = assets.load(FILE_TYPES_ASSET).ok().flatten();

        if let Some(content) = result {
            match std::str::from_utf8(&content) {
                Ok(content_str) => match serde_json::from_str::<FileTypesConfig>(content_str) {
                    Ok(config) => {
                        debug!("[FileIcons]: Successfully parsed config: {} stems, {} suffixes, {} types",
                                     config.stems.len(), config.suffixes.len(), config.types.len());
                        return config;
                    }
                    Err(err) => {
                        warn!("[FileIcons]: Error parsing file_types.json: {}", err);
                    }
                },
                Err(err) => {
                    warn!(
                        "[FileIcons]: Error converting file_types.json to UTF-8: {}",
                        err
                    );
                }
            }
        } else {
            warn!("[FileIcons]: Could not load file_types.json");
        }

        warn!("[FileIcons]: Using default empty config");
        FileTypesConfig::default()
    }

    /// Get the appropriate icon for a file path
    pub fn get_icon(&self, path: &Path) -> Option<SharedString> {
        // Extract the stem or suffix from the path
        let suffix = match path.icon_stem_or_suffix() {
            Some(s) => s,
            None => {
                warn!("[FileIcons]: No suffix found for path: {:?}", path);
                return self.get_type_icon("default");
            }
        };

        // First check if we have a match in the stems mapping
        if let Some(type_str) = self.config.stems.get(suffix) {
            trace!(
                "[FileIcons]: Found stem match: '{}' -> '{}'",
                suffix,
                type_str
            );
            return self.get_type_icon(type_str);
        }

        // Then check if we have a match in the suffixes mapping
        if let Some(type_str) = self.config.suffixes.get(suffix) {
            trace!(
                "[FileIcons]: Found suffix match: '{}' -> '{}'",
                suffix,
                type_str
            );
            return self.get_type_icon(type_str);
        }

        // Try fallback stems for specific filenames
        if let Some(filename) = path.file_name() {
            if let Some(filename_str) = filename.to_str() {
                if let Some(icon) = self.fallback_stems.get(filename_str) {
                    debug!(
                        "[FileIcons]: Using fallback stem icon for: '{}'",
                        filename_str
                    );
                    return Some(SharedString::from(icon.clone()));
                }
            }
        }

        // Try fallback suffixes for extensions
        if let Some(fallback) = self.fallback_suffixes.get(suffix) {
            debug!("[FileIcons]: Using fallback suffix icon for: '{}'", suffix);
            return Some(SharedString::from(fallback.clone()));
        }

        // Default icon
        debug!("[FileIcons]: Using default icon for: {:?}", path);
        self.get_type_icon("default")
    }

    /// Get icon based on type name
    fn get_type_icon(&self, typ: &str) -> Option<SharedString> {
        let result = self
            .config
            .types
            .get(typ)
            .map(|type_config| type_config.icon.clone());

        match &result {
            Some(icon) => trace!("[FileIcons]: Found icon for type '{}': '{}'", typ, icon),
            None => warn!("[FileIcons]: No icon found for type: '{}'", typ),
        }

        result
    }

    /// Get folder icon based on expanded state
    pub fn get_folder_icon(&self, expanded: bool) -> Option<SharedString> {
        let key = if expanded {
            EXPANDED_DIRECTORY_TYPE
        } else {
            COLLAPSED_DIRECTORY_TYPE
        };

        self.get_type_icon(key)
    }

    /// Get chevron icon for folders
    pub fn get_chevron_icon(&self, expanded: bool) -> Option<SharedString> {
        let key = if expanded {
            EXPANDED_CHEVRON_TYPE
        } else {
            COLLAPSED_CHEVRON_TYPE
        };

        self.get_type_icon(key)
    }

    /// Get working memory icon
    pub fn get_working_memory_icon(&self) -> Option<SharedString> {
        self.get_type_icon(WORKING_MEMORY_TYPE)
    }

    /// Get library icon for resources
    pub fn get_library_icon(&self) -> Option<SharedString> {
        self.get_type_icon("library")
    }

    /// Get file tree icon
    pub fn get_file_tree_icon(&self) -> Option<SharedString> {
        self.get_type_icon("file_tree")
    }

    /// Get arrow icon for toggling
    pub fn get_arrow_icon(&self, expanded: bool) -> Option<SharedString> {
        let icon_type = if expanded {
            "chevron_left"
        } else {
            "chevron_right"
        };

        if let Some(type_config) = self.config.types.get(icon_type) {
            Some(type_config.icon.clone())
        } else {
            // Fallback to text arrows
            if expanded {
                Some(SharedString::from("◀"))
            } else {
                Some(SharedString::from("▶"))
            }
        }
    }

    /// Get icon for web search
    pub fn get_search_icon(&self) -> Option<SharedString> {
        if let Some(type_config) = self.config.types.get("magnifying_glass") {
            Some(type_config.icon.clone())
        } else {
            Some(SharedString::from("🔍"))
        }
    }

    /// Get icon for web page
    pub fn get_web_icon(&self) -> Option<SharedString> {
        if let Some(type_config) = self.config.types.get("html") {
            Some(type_config.icon.clone())
        } else {
            Some(SharedString::from("🌐"))
        }
    }
}

// Singleton instance
static INSTANCE: OnceLock<FileIcons> = OnceLock::new();

/// Initialize the file icons system with the given asset source
pub fn init_with_assets(assets: &Arc<dyn AssetSource>) {
    INSTANCE.get_or_init(|| FileIcons::new(assets.clone()));
}

/// Initialize the file icons system using assets from the App
pub fn init(cx: &App) {
    trace!("[FileIcons]: Initializing file icons");
    let asset_source = cx.asset_source();
    init_with_assets(asset_source);
}

/// Get the FileIcons instance
pub fn get() -> &'static FileIcons {
    INSTANCE.get().expect("FileIcons not initialized")
}
