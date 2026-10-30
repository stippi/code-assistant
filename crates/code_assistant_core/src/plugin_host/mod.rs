//! Plugins: git repositories (or local folders) that extend the app without
//! recompiling it. A plugin's manifest (`.code-assistant-plugin/plugin.json`)
//! contributes
//!
//! - **provider templates**: prefilled providers offered when adding one,
//! - **MCP servers**: added to every registry while the plugin is on, not
//!   editable in the settings,
//! - **file renderers**: ways to show files in the Files view, a command
//!   turning a file into an image or a web editor editing it in place, with
//!   an optional script that starts a server they need ([`server`]).
//!
//! [`store`] installs, updates and removes plugins; [`catalog`] is what the
//! rest of the app reads. See `docs/plugins.md`.
//!
//! (Not to be confused with [`crate::plugins`], the compiled-in agent hooks.)

pub mod catalog;
pub mod manifest;
pub mod server;
pub mod store;

pub use catalog::{
    Catalog, Plugin, PluginMcpServers, PluginProviderTemplate, PluginRenderer, catalog,
};
pub use manifest::{
    MANIFEST_PATH, PluginFileRenderer, PluginManifest, ProviderTemplate, RendererKind,
    RendererOutput, ServerStart, ServerStop,
};
pub use server::{ServerLease, ServerTarget};
pub use store::{InstalledPlugin, PluginListing, PluginRecord, PluginSource, PluginStore};
