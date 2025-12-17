use gpui::{div, px, svg, App, AssetSource, IntoElement, ParentElement, SharedString, Styled};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use tracing::{debug, trace, warn};

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
    /// Set of already logged missing icon paths to avoid duplicate warnings
    logged_missing_icons: Mutex<HashSet<String>>,
}

// Public icon type constants
pub const CHEVRON_DOWN: &str = "chevron_down"; // chevron_down.svg
pub const CHEVRON_UP: &str = "chevron_up"; // chevron_up.svg
pub const WORKING_MEMORY: &str = "brain"; // brain.svg

pub const SEND: &str = "send"; // send.svg
pub const STOP: &str = "stop"; // circle_stop.svg
pub const MESSAGE_BUBBLES: &str = "message_bubbles"; // message_bubbles.svg
pub const PLUS: &str = "plus"; // plus.svg

// Tool-specific icon mappings to actual SVG files
pub const TOOL_READ_FILES: &str = "search_code"; // search_code.svg
pub const TOOL_LIST_FILES: &str = "reveal"; // reveal.svg
pub const TOOL_EXECUTE_COMMAND: &str = "terminal"; // terminal.svg
pub const TOOL_WRITE_FILE: &str = "pencil"; // pencil.svg
pub const TOOL_REPLACE_IN_FILE: &str = "replace"; // replace.svg
pub const TOOL_EDIT: &str = "edit"; // square-pen.svg
pub const TOOL_SEARCH_FILES: &str = "magnifying_glass"; // magnifying_glass.svg
pub const TOOL_GLOB_FILES: &str = "file_tree"; // file_tree.svg - for finding files by pattern
pub const TOOL_WEB_SEARCH: &str = "magnifying_glass"; // magnifying_glass.svg
pub const TOOL_WEB_FETCH: &str = "template"; // html.svg (use template/html as fallback)
pub const TOOL_DELETE_FILES: &str = "trash"; // trash.svg
pub const TOOL_OPEN_PROJECT: &str = "expanded_folder"; // folder_open.svg
pub const TOOL_USER_INPUT: &str = "person"; // person.svg
pub const TOOL_COMPLETE_TASK: &str = "check_circle"; // check_circle.svg
pub const TOOL_UPDATE_PLAN: &str = "file_generic"; // file_generic.svg
pub const TOOL_SPAWN_AGENT: &str = "rerun"; // rerun.svg - for spawning sub-agents
pub const TOOL_GENERIC: &str = "file_code"; // file_code.svg

const FILE_TYPES_ASSET: &str = "icons/file_icons/file_types.json";

impl FileIcons {
    /// Create a new FileIcons instance using the given AssetSource
    pub fn new(assets: Arc<dyn AssetSource>) -> Self {
        // Load the configuration from the JSON file
        let config = Self::load_config(&assets);

        Self {
            config,
            logged_missing_icons: Mutex::new(HashSet::new()),
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

    /// Log a missing icon only if it hasn't been logged before
    fn log_missing_icon(&self, message: &str, identifier: &str) {
        let mut logged = self.logged_missing_icons.lock().unwrap();
        if !logged.contains(identifier) {
            warn!("{}", message);
            logged.insert(identifier.to_string());
        }
    }

    /// Get icon based on type name - this is the core method that all icon lookups use
    pub fn get_type_icon(&self, typ: &str) -> Option<SharedString> {
        // First check if the type exists in the config
        let result = self
            .config
            .types
            .get(typ)
            .map(|type_config| type_config.icon.clone());

        if result.is_some() {
            trace!("[FileIcons]: Found icon for type '{}': '{:?}'", typ, result);
            return result;
        }

        // If not found in config, handle special tool types with hardcoded paths
        // These are SVG paths that don't depend on file_types.json
        let icon_path = match typ {
            TOOL_READ_FILES => Some("icons/search_code.svg"),
            TOOL_LIST_FILES => Some("icons/reveal.svg"),
            TOOL_EXECUTE_COMMAND => Some("icons/terminal.svg"),
            TOOL_WRITE_FILE => Some("icons/pencil.svg"),
            TOOL_REPLACE_IN_FILE => Some("icons/replace.svg"),
            TOOL_SEARCH_FILES => Some("icons/magnifying_glass.svg"),
            TOOL_GLOB_FILES => Some("icons/file_tree.svg"),
            // TOOL_WEB_SEARCH uses the same icon as SEARCH_FILES but is handled in the match condition above
            TOOL_DELETE_FILES => Some("icons/trash.svg"),
            TOOL_USER_INPUT => Some("icons/person.svg"),
            TOOL_COMPLETE_TASK => Some("icons/check_circle.svg"),

            TOOL_UPDATE_PLAN => Some("icons/file_generic.svg"),
            TOOL_SPAWN_AGENT => Some("icons/rerun.svg"),
            TOOL_GENERIC => Some("icons/file_code.svg"),
            // For file_types.json types we missed
            _ => None,
        };

        if let Some(path) = icon_path {
            trace!(
                "[FileIcons]: Using direct path for tool icon: '{}' -> '{}'",
                typ,
                path
            );
            return Some(SharedString::from(path));
        }

        // Finally, if everything else fails, fall back to emojis
        self.log_missing_icon(
            &format!("[FileIcons]: No icon found for type: '{typ}'"),
            &format!("no_icon_type:{typ}"),
        );

        match typ {
            TOOL_READ_FILES => Some(SharedString::from("📄")),
            TOOL_LIST_FILES => Some(SharedString::from("📂")),
            TOOL_EXECUTE_COMMAND => Some(SharedString::from("🖥️")),
            TOOL_WRITE_FILE => Some(SharedString::from("✏️")),
            TOOL_REPLACE_IN_FILE => Some(SharedString::from("🔄")),
            TOOL_EDIT => Some(SharedString::from("✏️")), // Pen/edit icon
            TOOL_SEARCH_FILES => Some(SharedString::from("🔍")),
            TOOL_GLOB_FILES => Some(SharedString::from("📁")),
            // TOOL_WEB_SEARCH uses same icon as SEARCH_FILES - handled above
            TOOL_WEB_FETCH => Some(SharedString::from("📥")),
            TOOL_DELETE_FILES => Some(SharedString::from("🗑️")),
            TOOL_OPEN_PROJECT => Some(SharedString::from("📂")),
            TOOL_USER_INPUT => Some(SharedString::from("👤")),
            TOOL_COMPLETE_TASK => Some(SharedString::from("✅")),

            TOOL_UPDATE_PLAN => Some(SharedString::from("📝")),
            TOOL_SPAWN_AGENT => Some(SharedString::from("🔄")),
            TOOL_GENERIC => Some(SharedString::from("🔧")),
            _ => Some(SharedString::from("📄")), // Default fallback
        }
    }

    /// Get tool-specific icon based on tool name
    pub fn get_tool_icon(&self, tool_name: &str) -> Option<SharedString> {
        let icon_type = match tool_name {
            "read_files" => TOOL_READ_FILES,
            "list_files" => TOOL_LIST_FILES,
            "execute_command" => TOOL_EXECUTE_COMMAND,
            "write_file" => TOOL_WRITE_FILE,
            "replace_in_file" => TOOL_REPLACE_IN_FILE,
            "edit" => TOOL_EDIT,
            "search_files" => TOOL_SEARCH_FILES,
            "glob_files" => TOOL_GLOB_FILES,
            "web_search" => TOOL_WEB_SEARCH,
            "web_fetch" => TOOL_WEB_FETCH,
            "delete_files" => TOOL_DELETE_FILES,
            "open_project" => TOOL_OPEN_PROJECT,
            "user_input" => TOOL_USER_INPUT,

            "complete_task" => TOOL_COMPLETE_TASK,
            "update_plan" => TOOL_UPDATE_PLAN,
            "spawn_agent" => TOOL_SPAWN_AGENT,
            _ => TOOL_GENERIC,
        };

        self.get_type_icon(icon_type)
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

/// Renders an icon as a gpui element based on the icon string and additional options
///
/// # Arguments
/// * `icon_opt` - Option<SharedString> with the icon string or None
/// * `size` - Size of the icon in pixels (default: 16.0)
/// * `color` - Color of the icon (default: hsla(0., 0., 0.7, 1.0))
/// * `fallback` - Fallback string to use when no icon is available (default: "📄")
///
/// # Returns
/// * A gpui element that represents the icon
pub fn render_icon(
    icon_opt: &Option<SharedString>,
    size: f32,
    color: impl Into<gpui::Hsla>,
    fallback: &str,
) -> gpui::AnyElement {
    let size_px = px(size);

    if let Some(icon_str) = icon_opt {
        if icon_str.starts_with("icons/") {
            // SVG icon
            svg()
                .size(size_px)
                .path(icon_str)
                .text_color(color)
                .into_any_element()
        } else {
            // Text/emoji icon
            div()
                .text_color(color)
                .child(icon_str.clone())
                .into_any_element()
        }
    } else {
        // Fallback icon
        div()
            .text_color(color)
            .child(fallback.to_string())
            .into_any_element()
    }
}

/// Renders an icon within a div container with specific dimensions
///
/// # Arguments
/// * `icon_opt` - Option<SharedString> with the icon string or None
/// * `size` - Size of the icon and container in pixels (default: 16.0)
/// * `color` - Color of the icon (default: hsla(0., 0., 0.7, 1.0))
/// * `fallback` - Fallback string to use when no icon is available (default: "📄")
///
/// # Returns
/// * A Div element that contains the icon in a container with the specified size
pub fn render_icon_container(
    icon_opt: &Option<SharedString>,
    size: f32,
    color: impl Into<gpui::Hsla>,
    fallback: &str,
) -> gpui::Div {
    let size_px = px(size);

    div()
        .w(size_px)
        .h(size_px)
        .flex()
        .items_center()
        .justify_center()
        .child(render_icon(icon_opt, size, color, fallback))
}
