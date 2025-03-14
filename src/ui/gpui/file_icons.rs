use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use gpui::SharedString;
use serde::Deserialize;

/// Represents icon information for different file types
#[derive(Deserialize, Debug)]
struct TypeConfig {
    icon: SharedString,
}

/// Configuration for file type associations
#[derive(Deserialize, Debug)]
struct FileTypesConfig {
    stems: HashMap<String, String>,
    suffixes: HashMap<String, String>,
    types: HashMap<String, TypeConfig>, 
}

/// A provider for file icons that supports SVG references
pub struct FileIcons {
    // The loaded configuration from file_types.json
    config: Option<FileTypesConfig>,
    // Base path to the svg icons
    icons_path: PathBuf,
    // Fallback emoji icons for when SVGs aren't available
    fallback_stems: HashMap<String, String>,
    fallback_suffixes: HashMap<String, String>,
}

const COLLAPSED_DIRECTORY_TYPE: &str = "collapsed_folder";
const EXPANDED_DIRECTORY_TYPE: &str = "expanded_folder";
const CHEVRON_RIGHT: &str = "chevron_right";
const CHEVRON_LEFT: &str = "chevron_left";
const DEFAULT_TYPE: &str = "default";
const HTML_TYPE: &str = "html";
const MAGNIFYING_GLASS_TYPE: &str = "magnifying_glass";

impl FileIcons {
    pub fn new() -> Self {
        // Try to load the configuration from the JSON file
        let config = Self::load_config();
        
        // Set up the base path to the SVG icons
        let icons_path = PathBuf::from("assets/icons/file_icons");
        
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
            icons_path,
            fallback_stems,
            fallback_suffixes,
        }
    }
    
    /// Load configuration from file_types.json
    fn load_config() -> Option<FileTypesConfig> {
        let config_path = PathBuf::from("assets/icons/file_icons/file_types.json");
        
        if let Ok(content) = fs::read_to_string(config_path) {
            match serde_json::from_str::<FileTypesConfig>(&content) {
                Ok(config) => return Some(config),
                Err(err) => {
                    eprintln!("Error parsing file_types.json: {}", err);
                }
            }
        } else {
            eprintln!("Could not read file_types.json");
        }
        
        None
    }
    
    /// Check if an SVG file exists at the given path
    fn svg_exists(&self, path: &str) -> bool {
        let full_path = self.icons_path.join(path);
        full_path.exists()
    }
    
    /// Get the path to the SVG file for a given type
    fn get_svg_path(&self, type_name: &str) -> Option<SharedString> {
        // First check if the config has this type
        if let Some(config) = &self.config {
            if let Some(type_config) = config.types.get(type_name) {
                // Return the icon path from the config
                return Some(type_config.icon.clone());
            }
        }
        
        // If not in config, try a direct mapping to an SVG file
        let svg_filename = format!("{}.svg", type_name);
        if self.svg_exists(&svg_filename) {
            return Some(SharedString::from(format!("icons/file_icons/{}", svg_filename)));
        }
        
        None
    }
    
    /// Get absolute path to an SVG file for debugging
    fn get_absolute_svg_path(&self, type_name: &str) -> Option<PathBuf> {
        let svg_filename = format!("{}.svg", type_name);
        let full_path = self.icons_path.join(&svg_filename);
        if full_path.exists() {
            Some(full_path)
        } else {
            None
        }
    }
    
    /// Get the appropriate icon for a file path
    pub fn get_icon(&self, path: &Path) -> SharedString {
        // Try to get icon by filename first
        if let Some(filename) = path.file_name() {
            if let Some(filename_str) = filename.to_str() {
                // Check if we have this specific filename in the stems mapping from config
                if let Some(config) = &self.config {
                    if let Some(type_name) = config.stems.get(filename_str) {
                        if let Some(svg_path) = self.get_svg_path(type_name) {
                            return svg_path;
                        }
                    }
                }
                
                // Try fallback stems
                if let Some(icon) = self.fallback_stems.get(filename_str) {
                    return SharedString::from(icon.clone());
                }
            }
        }
        
        // Then try by extension
        if let Some(extension) = path.extension() {
            if let Some(ext_str) = extension.to_str() {
                // Check if we have this extension in the suffixes mapping from config
                if let Some(config) = &self.config {
                    if let Some(type_name) = config.suffixes.get(ext_str) {
                        if let Some(svg_path) = self.get_svg_path(type_name) {
                            return svg_path;
                        }
                    }
                }
                
                // Try fallback suffixes
                if let Some(icon) = self.fallback_suffixes.get(&ext_str.to_lowercase()) {
                    return SharedString::from(icon.clone());
                }
            }
        }
        
        // Default file icon - try to get from SVG first
        if let Some(svg_path) = self.get_svg_path(DEFAULT_TYPE) {
            svg_path
        } else {
            // Fallback to emoji
            SharedString::from("📄")
        }
    }
    
    /// Get folder icon based on expanded state
    pub fn get_folder_icon(&self, expanded: bool) -> SharedString {
        let icon_type = if expanded {
            EXPANDED_DIRECTORY_TYPE
        } else {
            COLLAPSED_DIRECTORY_TYPE
        };
        
        if let Some(svg_path) = self.get_svg_path(icon_type) {
            svg_path
        } else {
            // Fallback to emoji
            if expanded {
                SharedString::from("📂")
            } else {
                SharedString::from("📁")
            }
        }
    }

    /// Get arrow icon for toggling 
    pub fn get_arrow_icon(&self, expanded: bool) -> SharedString {
        let icon_type = if expanded {
            CHEVRON_LEFT
        } else {
            CHEVRON_RIGHT
        };
        
        if let Some(svg_path) = self.get_svg_path(icon_type) {
            svg_path
        } else {
            // Fallback to text arrows
            if expanded {
                SharedString::from("◀")
            } else {
                SharedString::from("▶")
            }
        }
    }
    
    /// Get icon for web search
    pub fn get_search_icon(&self) -> SharedString {
        if let Some(svg_path) = self.get_svg_path(MAGNIFYING_GLASS_TYPE) {
            svg_path
        } else {
            SharedString::from("🔍")
        }
    }
    
    /// Get icon for web page
    pub fn get_web_icon(&self) -> SharedString {
        if let Some(svg_path) = self.get_svg_path(HTML_TYPE) {
            svg_path
        } else {
            SharedString::from("🌐")
        }
    }
}

// Singleton instance
static INSTANCE: OnceLock<FileIcons> = OnceLock::new();

pub fn init() {
    INSTANCE.get_or_init(|| FileIcons::new());
}

pub fn get() -> &'static FileIcons {
    INSTANCE.get_or_init(|| FileIcons::new())
}
