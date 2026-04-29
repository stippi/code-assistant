//! Persistent UI settings for the GPUI interface.
//!
//! Stores theme, font scale, and window bounds to disk so they survive restarts.

use gpui::{px, Bounds, Pixels, Point, Size};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, warn};

/// Serializable representation of window bounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowBoundsSettings {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl WindowBoundsSettings {
    pub fn from_gpui_bounds(bounds: Bounds<Pixels>) -> Self {
        Self {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
        }
    }

    pub fn to_gpui_bounds(&self) -> Bounds<Pixels> {
        Bounds {
            origin: Point {
                x: px(self.x),
                y: px(self.y),
            },
            size: Size {
                width: px(self.width),
                height: px(self.height),
            },
        }
    }

    /// Returns `true` if the stored bounds have a reasonable size (both dimensions > 100px).
    pub fn is_valid(&self) -> bool {
        self.width > 100.0 && self.height > 100.0
    }
}

/// Theme mode stored in settings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeModeSetting {
    Light,
    Dark,
}

/// Persistent UI settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSettings {
    /// Light or dark theme.
    #[serde(default = "default_theme_mode")]
    pub theme_mode: ThemeModeSetting,

    /// UI scale factor (1.0 = 100%).
    #[serde(default = "default_ui_scale")]
    pub ui_scale: f32,

    /// Window position and size (if previously saved).
    #[serde(default)]
    pub window_bounds: Option<WindowBoundsSettings>,
}

fn default_theme_mode() -> ThemeModeSetting {
    ThemeModeSetting::Dark
}

fn default_ui_scale() -> f32 {
    1.0
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            theme_mode: default_theme_mode(),
            ui_scale: default_ui_scale(),
            window_bounds: None,
        }
    }
}

impl UiSettings {
    /// Path to the settings file.
    fn settings_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| std::env::current_dir().unwrap())
            .join("code-assistant")
            .join("ui-settings.json")
    }

    /// Load settings from disk, returning defaults if the file is missing or invalid.
    pub fn load() -> Self {
        let path = Self::settings_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str(&contents) {
                Ok(settings) => {
                    debug!("Loaded UI settings from {}", path.display());
                    settings
                }
                Err(e) => {
                    warn!(
                        "Failed to parse UI settings from {}: {}. Using defaults.",
                        path.display(),
                        e
                    );
                    Self::default()
                }
            },
            Err(_) => {
                debug!("No UI settings file at {}. Using defaults.", path.display());
                Self::default()
            }
        }
    }

    /// Persist settings to disk. Errors are logged but not propagated.
    pub fn save(&self) {
        let path = Self::settings_path();
        match crate::utils::file_utils::atomic_write_json(&path, self) {
            Ok(()) => {
                debug!("Saved UI settings to {}", path.display());
            }
            Err(e) => {
                warn!("Failed to save UI settings to {}: {}", path.display(), e);
            }
        }
    }
}
