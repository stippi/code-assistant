use std::collections::HashMap;
use std::path::Path;
use gpui::SharedString;

/// A simple provider for file icons that returns string icons (emoji)
pub struct FileIcons {
    stems: HashMap<String, String>,
    suffixes: HashMap<String, String>,
    default_file_icon: String,
    folder_icon: String,
    folder_open_icon: String,
}

impl FileIcons {
    pub fn new() -> Self {
        let mut stems = HashMap::new();
        let mut suffixes = HashMap::new();
        
        // Initialize with common file types
        suffixes.insert("rs".to_string(), "🦀".to_string());
        suffixes.insert("js".to_string(), "📜".to_string());
        suffixes.insert("mjs".to_string(), "📜".to_string()); 
        suffixes.insert("jsx".to_string(), "⚛️".to_string());
        suffixes.insert("ts".to_string(), "📘".to_string());
        suffixes.insert("tsx".to_string(), "⚛️".to_string());
        suffixes.insert("py".to_string(), "🐍".to_string());
        suffixes.insert("html".to_string(), "🌐".to_string());
        suffixes.insert("htm".to_string(), "🌐".to_string());
        suffixes.insert("css".to_string(), "🎨".to_string());
        suffixes.insert("json".to_string(), "📋".to_string());
        suffixes.insert("md".to_string(), "📝".to_string());
        suffixes.insert("txt".to_string(), "📄".to_string());
        suffixes.insert("jpg".to_string(), "🖼️".to_string());
        suffixes.insert("jpeg".to_string(), "🖼️".to_string());
        suffixes.insert("png".to_string(), "🖼️".to_string());
        suffixes.insert("svg".to_string(), "🖌️".to_string());
        suffixes.insert("c".to_string(), "🔨".to_string());
        suffixes.insert("cpp".to_string(), "🔨".to_string());
        suffixes.insert("h".to_string(), "📐".to_string());
        suffixes.insert("hpp".to_string(), "📐".to_string());
        suffixes.insert("go".to_string(), "🐹".to_string());
        suffixes.insert("java".to_string(), "☕".to_string());
        suffixes.insert("php".to_string(), "🐘".to_string());
        suffixes.insert("rb".to_string(), "💎".to_string());
        suffixes.insert("sh".to_string(), "🐚".to_string());
        suffixes.insert("bash".to_string(), "🐚".to_string());
        suffixes.insert("toml".to_string(), "⚙️".to_string());
        suffixes.insert("yaml".to_string(), "⚙️".to_string());
        suffixes.insert("yml".to_string(), "⚙️".to_string());
        suffixes.insert("sql".to_string(), "🗃️".to_string());
        suffixes.insert("db".to_string(), "🗃️".to_string());
        suffixes.insert("pdf".to_string(), "📑".to_string());
        suffixes.insert("mp3".to_string(), "🎵".to_string());
        suffixes.insert("wav".to_string(), "🎵".to_string());
        suffixes.insert("mp4".to_string(), "🎬".to_string());
        suffixes.insert("csv".to_string(), "📊".to_string());
        suffixes.insert("lock".to_string(), "🔒".to_string());
        
        // Special file stems
        stems.insert("Cargo.toml".to_string(), "📦".to_string());
        stems.insert("package.json".to_string(), "📦".to_string());
        stems.insert("Dockerfile".to_string(), "🐳".to_string());
        stems.insert("docker-compose.yml".to_string(), "🐳".to_string());
        stems.insert("README.md".to_string(), "📚".to_string());
        stems.insert("LICENSE".to_string(), "⚖️".to_string());
        stems.insert(".gitignore".to_string(), "🔍".to_string());
        stems.insert(".env".to_string(), "🔐".to_string());
        
        Self {
            stems,
            suffixes,
            default_file_icon: "📄".to_string(),
            folder_icon: "📁".to_string(),
            folder_open_icon: "📂".to_string(),
        }
    }
    
    /// Get the appropriate icon for a file path
    pub fn get_icon(&self, path: &Path) -> SharedString {
        // Try by filename first
        if let Some(filename) = path.file_name() {
            if let Some(filename_str) = filename.to_str() {
                if let Some(icon) = self.stems.get(filename_str) {
                    return SharedString::from(icon.clone());
                }
            }
        }
        
        // Then try by extension
        if let Some(extension) = path.extension() {
            if let Some(ext_str) = extension.to_str() {
                if let Some(icon) = self.suffixes.get(&ext_str.to_lowercase()) {
                    return SharedString::from(icon.clone());
                }
            }
        }
        
        // Default file icon
        SharedString::from(self.default_file_icon.clone())
    }
    
    /// Get folder icon based on expanded state
    pub fn get_folder_icon(&self, expanded: bool) -> SharedString {
        if expanded {
            SharedString::from(self.folder_open_icon.clone())
        } else {
            SharedString::from(self.folder_icon.clone())
        }
    }

    /// Get arrow icon for toggling 
    pub fn get_arrow_icon(&self, expanded: bool) -> SharedString {
        if expanded {
            SharedString::from("◀")
        } else {
            SharedString::from("▶")
        }
    }
}

// Singleton instance
static INSTANCE: std::sync::OnceLock<FileIcons> = std::sync::OnceLock::new();

pub fn init() {
    INSTANCE.get_or_init(|| FileIcons::new());
}

pub fn get() -> &'static FileIcons {
    INSTANCE.get_or_init(|| FileIcons::new())
}
