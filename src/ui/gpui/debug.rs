// Debugging utilities for the UI components

use std::path::Path;
use gpui::SharedString;

// Simple logger helper for UI-related debugging
pub fn log_debug(context: &str, message: &str) {
    eprintln!("DEBUG [{context}]: {message}");
}

// Log attempt to load an asset
pub fn log_asset_load(path: &str, success: bool) {
    if success {
        log_debug("Assets", &format!("Successfully loaded asset: '{}'", path));
    } else {
        log_debug("Assets", &format!("Failed to load asset: '{}'", path));
    }
}

// Log rendering of icon
pub fn log_icon_render(icon: &Option<SharedString>, context: &str) {
    match icon {
        Some(icon_str) => log_debug(
            "Icon",
            &format!("Rendering icon '{}' in context '{}'", icon_str, context),
        ),
        None => log_debug("Icon", &format!("No icon to render in context '{}'", context)),
    }
}

// Debug file icon resolution
pub fn log_file_icon_resolution(path: &Path, icon_type: &str, icon: &Option<SharedString>) {
    let path_str = path.to_string_lossy();
    match icon {
        Some(icon_str) => log_debug(
            "FileIcons",
            &format!(
                "Resolved icon for '{}' (type: '{}'): '{}'",
                path_str, icon_type, icon_str
            ),
        ),
        None => log_debug(
            "FileIcons",
            &format!(
                "Failed to resolve icon for '{}' (type: '{}')",
                path_str, icon_type
            ),
        ),
    }
}
