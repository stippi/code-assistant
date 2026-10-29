//! The enabled plugins and what they contribute, as the rest of the app
//! reads them.
//!
//! [`catalog`] is cheap to call often (the Files view asks on every render):
//! it keeps the last load and checks the files it came from at most once a
//! second, reloading when `plugins.json` or a manifest changed. Changes made
//! through [`PluginStore`] take effect at once.

use super::manifest::{FileViewerSpec, PluginManifest, ProviderTemplate, substitute_root};
use super::store::{InstalledPlugin, PluginStore};
use mcp_client::{McpServerStatus, McpServersConfig, McpTransport};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// How often [`catalog`] looks for changed files.
const RECHECK_AFTER: Duration = Duration::from_secs(1);

/// An enabled plugin whose manifest loaded.
#[derive(Debug, Clone)]
pub struct Plugin {
    pub name: String,
    pub root: PathBuf,
    pub manifest: PluginManifest,
    pub installed: InstalledPlugin,
}

/// A provider template and the plugin offering it.
#[derive(Debug, Clone)]
pub struct PluginProviderTemplate {
    pub plugin: String,
    pub template: ProviderTemplate,
}

impl PluginProviderTemplate {
    /// Unique across plugins: `<plugin>/<template>`.
    pub fn key(&self) -> String {
        format!("{}/{}", self.plugin, self.template.id)
    }
}

/// A file viewer and the plugin offering it.
#[derive(Debug, Clone)]
pub struct PluginFileViewer {
    pub plugin: String,
    pub root: PathBuf,
    pub spec: FileViewerSpec,
}

impl PluginFileViewer {
    /// Unique across plugins: `<plugin>/<viewer>`.
    pub fn key(&self) -> String {
        format!("{}/{}", self.plugin, self.spec.id)
    }
}

/// The MCP servers plugins contribute, ready to merge.
#[derive(Debug, Default)]
pub struct PluginMcpServers {
    pub config: McpServersConfig,
    /// Which plugin each server comes from.
    pub owners: BTreeMap<String, String>,
    /// Servers left out (an unresolved `${VAR}`), as failed statuses.
    pub skipped: Vec<McpServerStatus>,
}

/// The enabled plugins, and the installed ones that failed to load.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub plugins: Vec<Plugin>,
    /// Plugin name → why it could not be loaded.
    pub errors: BTreeMap<String, String>,
    /// Changes whenever a plugin's contributions may have changed.
    fingerprint: String,
}

impl Catalog {
    /// Load the enabled plugins of `store`.
    pub fn load(store: &PluginStore) -> Self {
        let raw_record = std::fs::read_to_string(store.record_path()).unwrap_or_default();
        let mut fingerprint = raw_record;
        let mut catalog = Catalog::default();
        for (name, installed) in store.record().plugins {
            if !installed.enabled {
                continue;
            }
            match PluginManifest::load(&installed.path) {
                Ok(manifest) if manifest.name == name => {
                    fingerprint.push('\0');
                    fingerprint.push_str(&serde_json::to_string(&manifest).unwrap_or_default());
                    catalog.plugins.push(Plugin {
                        name,
                        root: installed.path.clone(),
                        manifest,
                        installed,
                    });
                }
                Ok(manifest) => {
                    catalog.errors.insert(
                        name.clone(),
                        format!("its manifest names it '{}'", manifest.name),
                    );
                }
                Err(e) => {
                    tracing::warn!("Plugin '{name}' not loaded: {e:#}");
                    catalog.errors.insert(name, format!("{e:#}"));
                }
            }
        }
        catalog.fingerprint = format!("{:x}", md5::compute(fingerprint));
        catalog
    }

    /// Changes whenever the enabled plugins or their manifests change.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn provider_templates(&self) -> Vec<PluginProviderTemplate> {
        self.plugins
            .iter()
            .flat_map(|plugin| {
                plugin
                    .manifest
                    .provider_templates
                    .iter()
                    .map(|template| PluginProviderTemplate {
                        plugin: plugin.name.clone(),
                        template: template.clone(),
                    })
            })
            .collect()
    }

    pub fn file_viewers(&self) -> Vec<PluginFileViewer> {
        self.plugins
            .iter()
            .flat_map(|plugin| {
                plugin
                    .manifest
                    .file_viewers
                    .iter()
                    .map(|spec| PluginFileViewer {
                        plugin: plugin.name.clone(),
                        root: plugin.root.clone(),
                        spec: spec.clone(),
                    })
            })
            .collect()
    }

    /// The viewer `key` (`<plugin>/<viewer>`).
    pub fn file_viewer(&self, key: &str) -> Option<PluginFileViewer> {
        self.file_viewers().into_iter().find(|v| v.key() == key)
    }

    /// The plugins' MCP servers: `${PLUGIN_ROOT}` replaced in commands,
    /// arguments, URLs, env and header values, a relative `./` command
    /// taken from the plugin root, `PLUGIN_ROOT` set for stdio servers, and
    /// `${VAR}`s in env and header values resolved from the environment. A
    /// name two plugins use goes to the later one.
    pub fn mcp_servers(&self) -> PluginMcpServers {
        let mut servers = PluginMcpServers::default();
        for plugin in &self.plugins {
            let mut config = match plugin.manifest.mcp_servers_config() {
                Ok(config) => config,
                Err(e) => {
                    tracing::warn!("Plugin '{}': {e:#}", plugin.name);
                    continue;
                }
            };
            for server in config.servers.values_mut() {
                localize(&mut server.transport, &plugin.root);
            }
            let root = plugin.root.display().to_string();
            let dropped = config.substitute_env_values(|name| {
                if name == "PLUGIN_ROOT" {
                    Some(root.clone())
                } else {
                    std::env::var(name).ok()
                }
            });
            for (name, error) in dropped {
                tracing::warn!(
                    "Skipping MCP server '{name}' of plugin '{}': {error:#}",
                    plugin.name
                );
                servers.skipped.push(McpServerStatus {
                    server: name,
                    result: Err(format!("Not started: {error:#}")),
                });
            }
            for (name, server) in config.servers {
                if let Some(other) = servers.owners.insert(name.clone(), plugin.name.clone()) {
                    tracing::warn!(
                        "MCP server '{name}' of plugin '{}' replaces the one of plugin '{other}'",
                        plugin.name
                    );
                }
                servers.config.servers.insert(name, server);
            }
        }
        servers
    }
}

/// Make a plugin's server transport independent of the working directory.
fn localize(transport: &mut McpTransport, root: &Path) {
    match transport {
        McpTransport::Stdio { command, args, env } => {
            *command = substitute_root(command, root);
            if command.starts_with("./") || command.starts_with("../") {
                *command = root.join(&*command).display().to_string();
            }
            for arg in args.iter_mut() {
                *arg = substitute_root(arg, root);
            }
            env.entry("PLUGIN_ROOT".to_string())
                .or_insert_with(|| root.display().to_string());
        }
        McpTransport::Http { url, .. } => *url = substitute_root(url, root),
    }
}

/// What a catalog was loaded from, to notice changes cheaply.
type Stamp = Vec<(PathBuf, Option<(SystemTime, u64)>)>;

struct Cached {
    dir: PathBuf,
    catalog: Arc<Catalog>,
    stamp: Stamp,
    checked: Instant,
}

static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

fn stamp(store: &PluginStore, catalog: &Catalog) -> Stamp {
    let file_stamp = |path: PathBuf| {
        let stamp = std::fs::metadata(&path)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())));
        (path, stamp)
    };
    let mut files = vec![file_stamp(store.record_path())];
    // Every installed plugin's manifest, so fixing a broken one shows too.
    for installed in store.record().plugins.values() {
        files.push(file_stamp(
            installed.path.join(super::manifest::MANIFEST_PATH),
        ));
    }
    files.extend(
        catalog
            .plugins
            .iter()
            .map(|p| file_stamp(p.root.join(super::manifest::MANIFEST_PATH))),
    );
    files
}

/// The current catalog of the plugin store in the configuration directory.
pub fn catalog() -> Arc<Catalog> {
    let store = PluginStore::new();
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cached) = cache.as_mut()
        && cached.dir == store.dir()
    {
        if cached.checked.elapsed() < RECHECK_AFTER {
            return cached.catalog.clone();
        }
        cached.checked = Instant::now();
        if stamp(&store, &cached.catalog) == cached.stamp {
            return cached.catalog.clone();
        }
    }
    let catalog = Arc::new(Catalog::load(&store));
    *cache = Some(Cached {
        dir: store.dir().to_path_buf(),
        stamp: stamp(&store, &catalog),
        catalog: catalog.clone(),
        checked: Instant::now(),
    });
    catalog
}

/// Forget the cached catalog (the store changed).
pub(crate) fn invalidate() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_host::manifest::MANIFEST_PATH;

    fn plugin(dir: &Path, manifest: &str) {
        let path = dir.join(MANIFEST_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, manifest).unwrap();
    }

    #[test]
    fn only_enabled_plugins_contribute() {
        let temp = tempfile::tempdir().unwrap();
        let store = PluginStore::at(temp.path().join("plugins"));
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        plugin(
            &a,
            r#"{ "name": "a", "providerTemplates": [
                { "id": "t", "label": "T", "provider": "ollama",
                  "config": { "base_url": "http://localhost:11434" } }] }"#,
        );
        plugin(
            &b,
            r#"{ "name": "b", "fileViewers": [
                { "id": "v", "label": "V", "files": ["*.x"], "url": "http://localhost:1/" }] }"#,
        );
        store.install(a.to_str().unwrap()).unwrap();
        store.install(b.to_str().unwrap()).unwrap();

        let catalog = Catalog::load(&store);
        assert_eq!(catalog.provider_templates()[0].key(), "a/t");
        assert_eq!(catalog.file_viewers()[0].key(), "b/v");
        assert!(catalog.file_viewer("b/v").is_some());

        let before = catalog.fingerprint().to_string();
        store.set_enabled("b", false).unwrap();
        let catalog = Catalog::load(&store);
        assert!(catalog.file_viewers().is_empty());
        assert_ne!(catalog.fingerprint(), before);
    }

    #[test]
    fn mcp_servers_are_made_independent_of_the_working_directory() {
        let temp = tempfile::tempdir().unwrap();
        let store = PluginStore::at(temp.path().join("plugins"));
        let dir = temp.path().join("tools");
        plugin(
            &dir,
            r#"{ "name": "tools", "mcpServers": {
                "local": { "command": "./bin/server", "args": ["--root", "${PLUGIN_ROOT}/data"],
                           "env": { "HOME_DIR": "${PLUGIN_ROOT}" } },
                "remote": { "type": "http", "url": "https://example.com/mcp" },
                "secret": { "type": "http", "url": "https://example.com/mcp",
                            "headers": { "Authorization": "${PLUGIN_TEST_UNSET_VAR}" } }
            } }"#,
        );
        store.install(dir.to_str().unwrap()).unwrap();
        let root = std::fs::canonicalize(&dir).unwrap();
        let servers = Catalog::load(&store).mcp_servers();
        let McpTransport::Stdio { command, args, env } = &servers.config.servers["local"].transport
        else {
            panic!("stdio expected");
        };
        assert_eq!(Path::new(command), root.join("./bin/server"));
        assert_eq!(args[1], format!("{}/data", root.display()));
        assert_eq!(env["HOME_DIR"], root.display().to_string());
        assert_eq!(env["PLUGIN_ROOT"], root.display().to_string());
        assert_eq!(servers.owners["remote"], "tools");
        assert!(!servers.config.servers.contains_key("secret"));
        assert_eq!(servers.skipped[0].server, "secret");
    }
}
