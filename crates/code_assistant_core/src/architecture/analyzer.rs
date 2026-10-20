//! Keeps the overview of each project current at little cost.
//!
//! A scan re-reads only files whose size or modification time changed. A
//! summary is asked from the model only for a component whose digest has
//! no cached summary yet; summaries are stored per project under the
//! digest's key, so returning to an earlier state (another branch, a
//! reverted change) finds its summaries again.

use super::digest::{self, ComponentSummary};
use super::extract::{self, FileFacts, Language};
use super::scan::{self, ArchitectureModel, Manifest, SourceFile};
use crate::utils::file_utils::atomic_write_json;
use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tracing::warn;

/// Files above this size are counted, not read.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Summaries remembered per component (for returning to earlier states).
const ENTRIES_PER_COMPONENT: usize = 4;
/// Model calls running at once.
const CONCURRENT_CALLS: usize = 3;

/// The model's answer to one digest.
#[derive(Debug, Clone)]
pub struct Completion {
    pub text: String,
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Asks a model; the seam tests replace.
#[async_trait::async_trait]
pub trait Summarizer: Send + Sync {
    async fn complete(&self, system: &str, digest: &str) -> Result<Completion>;
}

/// A summary from the cache; `fresh` when made from the current digest.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedSummary {
    pub summary: ComponentSummary,
    pub fresh: bool,
}

/// A scan's result: the model plus every cached summary that applies.
#[derive(Debug, Clone)]
pub struct ArchitectureOverview {
    pub model: Arc<ArchitectureModel>,
    pub summaries: HashMap<String, CachedSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    key: String,
    summary: ComponentSummary,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SummaryCache {
    root: PathBuf,
    /// Component id → its summaries, newest first.
    entries: HashMap<String, Vec<CacheEntry>>,
}

impl SummaryCache {
    fn lookup(&self, id: &str, key: &str) -> Option<&ComponentSummary> {
        self.entries
            .get(id)?
            .iter()
            .find(|e| e.key == key)
            .map(|e| &e.summary)
    }

    fn latest(&self, id: &str) -> Option<&CacheEntry> {
        self.entries.get(id)?.first()
    }

    fn insert(&mut self, id: &str, key: String, summary: ComponentSummary) {
        let entries = self.entries.entry(id.to_owned()).or_default();
        entries.retain(|e| e.key != key);
        entries.insert(
            0,
            CacheEntry {
                key,
                summary,
                created_at: chrono::Utc::now(),
            },
        );
        entries.truncate(ENTRIES_PER_COMPONENT);
    }
}

struct CachedFile {
    modified: Option<SystemTime>,
    len: u64,
    facts: Arc<FileFacts>,
}

#[derive(Default)]
struct Project {
    files: HashMap<String, CachedFile>,
    model: Option<Arc<ArchitectureModel>>,
    cache: Option<SummaryCache>,
}

/// See module docs. Shared by all sessions; one state per project root.
pub struct Analyzer {
    summarizer: Arc<dyn Summarizer>,
    /// Where summaries persist; `None` keeps them in memory.
    cache_dir: Option<PathBuf>,
    projects: Mutex<HashMap<PathBuf, Arc<Mutex<Project>>>>,
    /// Digest keys being summarized, so one digest is asked once.
    inflight: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    calls: tokio::sync::Semaphore,
}

impl Analyzer {
    pub fn new(summarizer: Arc<dyn Summarizer>, cache_dir: Option<PathBuf>) -> Self {
        Self {
            summarizer,
            cache_dir,
            projects: Mutex::new(HashMap::new()),
            inflight: tokio::sync::Mutex::new(HashMap::new()),
            calls: tokio::sync::Semaphore::new(CONCURRENT_CALLS),
        }
    }

    fn project(&self, root: &Path) -> Arc<Mutex<Project>> {
        self.projects
            .lock()
            .unwrap()
            .entry(root.to_path_buf())
            .or_default()
            .clone()
    }

    fn cache_path(&self, root: &Path) -> Option<PathBuf> {
        let key = digest::digest_key(&root.to_string_lossy());
        Some(self.cache_dir.as_ref()?.join(format!("{key}.json")))
    }

    fn ensure_cache<'a>(&self, root: &Path, project: &'a mut Project) -> &'a mut SummaryCache {
        project.cache.get_or_insert_with(|| {
            self.cache_path(root)
                .and_then(|path| std::fs::read_to_string(path).ok())
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_else(|| SummaryCache {
                    root: root.to_path_buf(),
                    ..SummaryCache::default()
                })
        })
    }

    /// Scan the project at `root` (blocking: walks and reads files).
    pub fn scan(&self, root: &Path) -> Result<ArchitectureOverview> {
        let (paths, truncated) = fs_explorer::browse::list_files(root)?;
        let project = self.project(root);
        let mut previous = std::mem::take(&mut project.lock().unwrap().files);
        let mut files = HashMap::new();
        let mut sources = Vec::new();
        let mut manifests = HashMap::new();
        let mut readmes = HashMap::new();
        for path in paths.into_iter().filter(|p| scan::is_relevant_path(p)) {
            let (dir, name) = match path.rsplit_once('/') {
                Some((dir, name)) => (dir.to_owned(), name),
                None => (String::new(), path.as_str()),
            };
            let full = root.join(&path);
            if scan::is_manifest(name) {
                let text = std::fs::read_to_string(&full).unwrap_or_default();
                if let Some(manifest) = Manifest::parse(name, &text) {
                    manifests.insert(dir, manifest);
                }
                continue;
            }
            if name.eq_ignore_ascii_case("readme.md") {
                if let Ok(text) = std::fs::read_to_string(&full) {
                    readmes.insert(dir, text);
                }
                continue;
            }
            let Some(language) = Language::from_path(name) else {
                continue;
            };
            let Ok(meta) = std::fs::metadata(&full) else {
                continue;
            };
            let modified = meta.modified().ok();
            let facts = match previous.remove(&path) {
                Some(cached) if cached.modified == modified && cached.len == meta.len() => {
                    cached.facts
                }
                _ if meta.len() > MAX_FILE_BYTES => Arc::new(FileFacts::default()),
                _ => match std::fs::read(&full) {
                    Ok(bytes) => {
                        Arc::new(extract::extract(language, &String::from_utf8_lossy(&bytes)))
                    }
                    Err(_) => continue,
                },
            };
            files.insert(
                path.clone(),
                CachedFile {
                    modified,
                    len: meta.len(),
                    facts: facts.clone(),
                },
            );
            sources.push(SourceFile {
                path,
                language,
                facts,
            });
        }
        let model = Arc::new(scan::build_model(
            root.to_path_buf(),
            sources,
            manifests,
            readmes,
            truncated,
        ));

        let mut project = project.lock().unwrap();
        project.files = files;
        project.model = Some(model.clone());
        let cache = self.ensure_cache(root, &mut project);
        let mut summaries = HashMap::new();
        for (id, entries) in &cache.entries {
            let Some(latest) = entries.first() else {
                continue;
            };
            let Some(digest) = digest::build_digest(&model, id) else {
                continue;
            };
            let key = digest::digest_key(&digest);
            let summary = match entries.iter().find(|e| e.key == key) {
                Some(entry) => CachedSummary {
                    summary: entry.summary.clone(),
                    fresh: true,
                },
                None => CachedSummary {
                    summary: latest.summary.clone(),
                    fresh: false,
                },
            };
            summaries.insert(id.clone(), summary);
        }
        Ok(ArchitectureOverview { model, summaries })
    }

    /// The summary of component `id`, from the cache or the model. `force`
    /// asks the model even when the cache has one.
    pub async fn summarize(
        self: &Arc<Self>,
        root: &Path,
        id: &str,
        force: bool,
    ) -> Result<ComponentSummary> {
        let project = self.project(root);
        let model = project.lock().unwrap().model.clone();
        let model = match model {
            Some(model) => model,
            None => {
                let this = self.clone();
                let root = root.to_path_buf();
                tokio::task::spawn_blocking(move || this.scan(&root))
                    .await
                    .context("Architecture scan was aborted")??
                    .model
            }
        };
        let digest = digest::build_digest(&model, id)
            .ok_or_else(|| anyhow!("No component '{id}' in the project"))?;
        let key = digest::digest_key(&digest);
        let cached = |analyzer: &Self| {
            let mut project = project.lock().unwrap();
            analyzer
                .ensure_cache(root, &mut project)
                .lookup(id, &key)
                .cloned()
        };
        if !force && let Some(summary) = cached(self) {
            return Ok(summary);
        }

        let gate = self
            .inflight
            .lock()
            .await
            .entry(key.clone())
            .or_default()
            .clone();
        let _guard = gate.lock().await;
        // Someone else may have asked meanwhile.
        if !force && let Some(summary) = cached(self) {
            return Ok(summary);
        }
        let result = async {
            let _permit = self.calls.acquire().await?;
            let completion = self
                .summarizer
                .complete(digest::SYSTEM_PROMPT, &digest)
                .await?;
            let mut summary = digest::parse_summary(&completion.text)?;
            summary.model = completion.model;
            summary.input_tokens = completion.input_tokens;
            summary.output_tokens = completion.output_tokens;
            Ok::<_, anyhow::Error>(summary)
        }
        .await;
        self.inflight.lock().await.remove(&key);
        let summary = result?;

        let mut project = project.lock().unwrap();
        let cache = self.ensure_cache(root, &mut project);
        cache.insert(id, key, summary.clone());
        if let Some(path) = self.cache_path(root)
            && let Err(e) = atomic_write_json(&path, cache)
        {
            warn!("Failed to store architecture summaries: {e:#}");
        }
        Ok(summary)
    }

    /// The newest cached summary of `id`, whatever digest it was made from.
    pub fn latest_summary(&self, root: &Path, id: &str) -> Option<ComponentSummary> {
        let project = self.project(root);
        let mut project = project.lock().unwrap();
        self.ensure_cache(root, &mut project)
            .latest(id)
            .map(|e| e.summary.clone())
    }
}

/// Explains components with a model from `models.json`, chosen in
/// [`super::ArchitectureConfig`] at every call so settings apply at once.
pub struct LlmSummarizer {
    factory: Option<crate::session::service::LlmClientFactory>,
    model: Option<String>,
}

impl LlmSummarizer {
    /// `factory` overrides client construction (tests, playback).
    pub fn new(factory: Option<crate::session::service::LlmClientFactory>) -> Self {
        Self {
            factory,
            model: None,
        }
    }

    /// Always use `model`, whatever the settings say.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
}

#[async_trait::async_trait]
impl Summarizer for LlmSummarizer {
    async fn complete(&self, system: &str, digest: &str) -> Result<Completion> {
        let model = match &self.model {
            Some(model) => model.clone(),
            None => super::ArchitectureConfig::load().resolved_model().ok_or_else(|| {
                anyhow!(
                    "No model for the architecture overview: choose one in Settings → Architecture"
                )
            })?,
        };
        let mut client = match &self.factory {
            Some(factory) => factory(&model)?,
            None => llm::factory::create_llm_client_from_model(&model, None, false, None).await?,
        };
        let request = llm::LLMRequest {
            messages: vec![llm::Message::new_user(digest.to_owned())],
            system_prompt: system.to_owned(),
            session_id: "architecture-overview".into(),
            ..Default::default()
        };
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(180),
            client.send_message(request, None),
        )
        .await
        .map_err(|_| anyhow!("The model did not answer within 3 minutes"))??;
        let text = response
            .content
            .iter()
            .filter_map(|block| match block {
                llm::ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        Ok(Completion {
            text,
            model,
            input_tokens: response.usage.input_tokens,
            output_tokens: response.usage.output_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scripted {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Summarizer for Scripted {
        async fn complete(&self, _system: &str, digest: &str) -> Result<Completion> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let parts: serde_json::Map<String, serde_json::Value> = digest
                .lines()
                .filter_map(|l| l.strip_prefix("## "))
                .filter_map(|l| l.split(" (").next())
                .map(|name| (name.to_owned(), format!("does {name}").into()))
                .collect();
            Ok(Completion {
                text: serde_json::json!({
                    "summary": format!("call {n}"),
                    "parts": parts,
                })
                .to_string(),
                model: "scripted".into(),
                input_tokens: 100,
                output_tokens: 10,
            })
        }
    }

    fn write(root: &Path, path: &str, text: &str) {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }

    #[tokio::test]
    async fn summaries_are_cached_by_digest() {
        let project = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let root = project.path();
        write(root, "Cargo.toml", "[package]\nname = \"demo\"\n");
        write(root, "src/lib.rs", "pub mod a;\npub mod b;\n");
        write(root, "src/a.rs", "pub fn a() { 1; }\n");
        write(root, "src/b.rs", "use crate::a::a;\npub fn b() {}\n");

        let scripted = Arc::new(Scripted {
            calls: AtomicUsize::new(0),
        });
        let analyzer = Arc::new(Analyzer::new(
            scripted.clone(),
            Some(cache.path().to_path_buf()),
        ));
        let overview = analyzer.scan(root).unwrap();
        assert!(overview.summaries.is_empty());
        assert_eq!(overview.model.uses("src/b.rs"), vec!["src/a.rs"]);

        // Two views asking at once cost one call.
        let (first, second) = tokio::join!(
            analyzer.summarize(root, "", false),
            analyzer.summarize(root, "", false)
        );
        assert_eq!(first.unwrap().summary, "call 1");
        let summary = second.unwrap();
        assert_eq!(summary.summary, "call 1");
        assert_eq!(summary.parts["a.rs"], "does a.rs");
        assert_eq!(summary.input_tokens, 100);
        assert_eq!(scripted.calls.load(Ordering::SeqCst), 1);

        // A body edit leaves the digest alone.
        write(root, "src/a.rs", "pub fn a() { 2; }\n");
        let overview = analyzer.scan(root).unwrap();
        assert!(overview.summaries[""].fresh);
        analyzer.summarize(root, "", false).await.unwrap();
        assert_eq!(scripted.calls.load(Ordering::SeqCst), 1);

        // A new public item changes it: the old summary is stale until a
        // new one is made.
        write(
            root,
            "src/a.rs",
            "pub fn a() { 2; }\npub struct NewThing;\n",
        );
        let overview = analyzer.scan(root).unwrap();
        assert!(!overview.summaries[""].fresh);
        let summary = analyzer.summarize(root, "", false).await.unwrap();
        assert_eq!(summary.summary, "call 2");

        // Back to the earlier state: its summary is still known.
        write(root, "src/a.rs", "pub fn a() { 2; }\n");
        analyzer.scan(root).unwrap();
        assert_eq!(
            analyzer.summarize(root, "", false).await.unwrap().summary,
            "call 1"
        );

        // Summaries survive a restart; forcing asks again.
        let restarted = Arc::new(Analyzer::new(
            scripted.clone(),
            Some(cache.path().to_path_buf()),
        ));
        let overview = restarted.scan(root).unwrap();
        assert!(overview.summaries[""].fresh);
        assert_eq!(scripted.calls.load(Ordering::SeqCst), 2);
        let forced = restarted.summarize(root, "", true).await.unwrap();
        assert_eq!(forced.summary, "call 3");
    }
}
