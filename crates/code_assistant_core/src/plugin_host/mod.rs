//! Plugins: git repositories (or local folders) that extend the app without
//! recompiling it. A plugin's manifest (`.code-assistant-plugin/plugin.json`)
//! contributes
//!
//! - **provider templates**: prefilled providers offered when adding one,
//! - **MCP servers**: added to every registry while the plugin is on, not
//!   editable in the settings,
//! - **file viewers**: web pages that show (and may edit) files in the Files
//!   view, with an optional script that starts their server.
//!
//! [`store`] installs, updates and removes plugins; [`catalog`] is what the
//! rest of the app reads. See `docs/plugins.md`.
//!
//! (Not to be confused with [`crate::plugins`], the compiled-in agent hooks.)

pub mod catalog;
pub mod manifest;
pub mod store;
pub mod viewer;

pub use catalog::{
    Catalog, Plugin, PluginFileViewer, PluginMcpServers, PluginProviderTemplate, catalog,
};
pub use manifest::{
    FileViewerSpec, MANIFEST_PATH, PluginManifest, ProviderTemplate, ServerStart, ViewerProtocol,
};
pub use store::{InstalledPlugin, PluginListing, PluginRecord, PluginSource, PluginStore};
