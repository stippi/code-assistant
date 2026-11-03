//! [`UpdateService`]: the frontends' handle to check for, download and
//! install a newer release. One worker does one step at a time and checks
//! on its own after a delay and then periodically. State changes arrive as
//! app-scoped [`UiEvent::UpdateStateChanged`] events.

use super::state::{UpdateState, UpdateStatus, UpdateStep};
use crate::session::event_stream::EventStream;
use crate::ui::UiEvent;
use crate::version;
use anyhow::{Context, anyhow, bail};
use app_update::{
    Asset, InstallRecord, Installation, Manifest, RetryPolicy, UpdateClient, UpdateStore, Version,
};
use chrono::Local;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tracing::{info, warn};

const FIRST_CHECK: Duration = Duration::from_secs(15);
const CHECK_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);

pub struct UpdateConfig {
    /// URL of the update manifest; without one the build never updates.
    pub feed: Option<String>,
    pub disabled_by_env: bool,
    pub current_version: Version,
    pub installation: Installation,
    /// The release asset this installation needs.
    pub asset_name: Option<String>,
    pub store: UpdateStore,
    pub retry: RetryPolicy,
    pub first_check: Duration,
    pub check_interval: Duration,
}

impl UpdateConfig {
    /// This build's: the feed the release workflow compiled in (or
    /// `CODE_ASSISTANT_UPDATE_FEED`, for testing against a local server),
    /// the running installation, downloads in the user's cache folder.
    pub fn from_environment() -> anyhow::Result<Self> {
        let feed = std::env::var("CODE_ASSISTANT_UPDATE_FEED")
            .ok()
            .filter(|feed| !feed.is_empty())
            .or_else(|| version::update_feed().map(str::to_owned));
        let installation = Installation::current().context("locating the running app")?;
        let asset_name = super::asset_name(
            version::TARGET,
            installation.kind,
            app_update::running_under_rosetta(),
        );
        let updates_dir = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("code-assistant")
            .join("updates");
        Ok(Self {
            feed,
            disabled_by_env: std::env::var_os("CODE_ASSISTANT_DISABLE_AUTO_UPDATE").is_some(),
            current_version: Version::parse(version::VERSION)?,
            installation,
            asset_name,
            store: UpdateStore::new(updates_dir),
            retry: RetryPolicy::default(),
            first_check: FIRST_CHECK,
            check_interval: CHECK_INTERVAL,
        })
    }

    fn disabled_reason(&self) -> Option<String> {
        if self.feed.is_none() {
            return Some(
                "This build does not update itself; the release builds from GitHub do.".into(),
            );
        }
        if self.disabled_by_env {
            return Some("Updates are turned off by CODE_ASSISTANT_DISABLE_AUTO_UPDATE.".into());
        }
        if self.asset_name.is_none() {
            return Some("There is no release build for this platform.".into());
        }
        self.installation.unsupported_reason()
    }
}

enum Command {
    Check(oneshot::Sender<()>),
    Download(oneshot::Sender<()>),
    Install(oneshot::Sender<anyhow::Result<PathBuf>>),
}

/// Cloneable handle; the worker runs in the future returned by
/// [`UpdateService::new`], which the application spawns on its backend
/// runtime.
#[derive(Clone)]
pub struct UpdateService {
    tx: mpsc::UnboundedSender<Command>,
    state: Arc<Mutex<UpdateState>>,
}

impl UpdateService {
    /// Cleans up after earlier updates right away (removing downloads of
    /// versions not newer than this one, noticing an install that did not
    /// apply); checking starts with the worker.
    pub fn new(config: UpdateConfig, events: EventStream) -> (Self, impl Future<Output = ()>) {
        let disabled = config.disabled_reason();
        let mut initial = UpdateState::new(
            config.current_version.clone(),
            match &disabled {
                Some(reason) => UpdateStatus::Disabled {
                    reason: reason.clone(),
                },
                None => UpdateStatus::Idle,
            },
        );
        if disabled.is_none() {
            initial.not_applied = clean_up(&config);
        }
        let state = Arc::new(Mutex::new(initial));
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = Worker {
            publisher: Publisher {
                state: state.clone(),
                events,
            },
            config,
            offer: None,
        };
        (Self { tx, state }, worker.run(rx, disabled.is_none()))
    }

    pub fn state(&self) -> UpdateState {
        self.state.lock().unwrap().clone()
    }

    /// Asks for the newest release now.
    pub async fn check(&self) {
        let (reply, done) = oneshot::channel();
        if self.tx.send(Command::Check(reply)).is_ok() {
            let _ = done.await;
        }
    }

    /// Downloads the available release; failures end up in the state.
    pub async fn download(&self) {
        let (reply, done) = oneshot::channel();
        if self.tx.send(Command::Download(reply)).is_ok() {
            let _ = done.await;
        }
    }

    /// Swaps the downloaded release in. Returns what to start instead of the
    /// running app; the caller restarts right away.
    pub async fn install(&self) -> anyhow::Result<PathBuf> {
        let (reply, done) = oneshot::channel();
        self.tx
            .send(Command::Install(reply))
            .map_err(|_| anyhow!("the update service is not running"))?;
        done.await
            .map_err(|_| anyhow!("the update service stopped"))?
    }
}

/// Startup housekeeping; returns the version an install did not apply.
fn clean_up(config: &UpdateConfig) -> Option<Version> {
    app_update::remove_leftovers(&config.installation);
    let current = &config.current_version;
    let not_applied = match config.store.read_install_record() {
        Some(record) if record.from_version == *current && record.target_version != *current => {
            warn!(
                "The update to {} did not apply; still running {current}",
                record.target_version
            );
            Some(record.target_version)
        }
        Some(_) => {
            config.store.remove_install_record();
            None
        }
        None => None,
    };
    config.store.retain(|version| version > current);
    not_applied
}

/// Changes the shared state and tells the frontends.
#[derive(Clone)]
struct Publisher {
    state: Arc<Mutex<UpdateState>>,
    events: EventStream,
}

impl Publisher {
    fn status(&self) -> UpdateStatus {
        self.state.lock().unwrap().status.clone()
    }

    fn update(&self, change: impl FnOnce(&mut UpdateState)) {
        let state = {
            let mut state = self.state.lock().unwrap();
            change(&mut state);
            state.clone()
        };
        self.events
            .publish_app(UiEvent::UpdateStateChanged { state });
    }
}

struct Worker {
    publisher: Publisher,
    config: UpdateConfig,
    /// The release the last check offered.
    offer: Option<(Version, Asset)>,
}

impl Worker {
    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>, enabled: bool) {
        let client = if enabled {
            UpdateClient::new(&format!("code-assistant/{}", self.config.current_version))
                .map(|client| client.with_retry(self.config.retry))
                .inspect_err(|e| warn!("Updates are off: {e:#}"))
                .ok()
        } else {
            None
        };
        let mut next_check = Instant::now() + self.config.first_check;
        loop {
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    match command {
                        Command::Check(reply) => {
                            if let Some(client) = &client {
                                self.check(client).await;
                                next_check = Instant::now() + self.config.check_interval;
                            }
                            let _ = reply.send(());
                        }
                        Command::Download(reply) => {
                            if let Some(client) = &client {
                                self.download(client).await;
                            }
                            let _ = reply.send(());
                        }
                        Command::Install(reply) => {
                            let _ = reply.send(self.install().await);
                        }
                    }
                }
                () = tokio::time::sleep_until(next_check), if client.is_some() => {
                    if let Some(client) = &client {
                        self.check(client).await;
                    }
                    next_check = Instant::now() + self.config.check_interval;
                }
            }
        }
    }

    fn asset_name(&self) -> &str {
        self.config.asset_name.as_deref().unwrap_or_default()
    }

    async fn check(&mut self, client: &UpdateClient) {
        let previous = self.publisher.status();
        self.publisher
            .update(|state| state.status = UpdateStatus::Checking);
        let feed = self.config.feed.clone().unwrap_or_default();
        let result = match client.fetch_manifest(&feed).await {
            Ok(manifest) => self.offer(manifest).await,
            Err(e) => Err(e),
        };
        let checked_at = Local::now();
        self.publisher.update(|state| {
            state.checked_at = Some(checked_at);
            match result {
                Ok(status) => {
                    state.status = status;
                    state.clear_error(UpdateStep::Check);
                }
                Err(e) => {
                    warn!("Checking for updates failed: {e:#}");
                    state.status = previous;
                    state.fail(UpdateStep::Check, &e);
                }
            }
        });
    }

    /// What the manifest offers this installation.
    async fn offer(&mut self, manifest: Manifest) -> anyhow::Result<UpdateStatus> {
        let version = manifest.version.clone();
        if version <= self.config.current_version {
            self.offer = None;
            self.config.store.retain(|_| false);
            return Ok(UpdateStatus::UpToDate);
        }
        let asset = manifest
            .asset(self.asset_name())
            .with_context(|| format!("release {version} has no {}", self.asset_name()))?
            .clone();
        self.config.store.retain(|kept| *kept == version);
        let path = self.config.store.asset_path(&version, self.asset_name());
        let downloaded = app_update::is_verified(&path, &asset).await;
        info!("Release {version} is available (downloaded: {downloaded})");
        self.offer = Some((version.clone(), asset));
        Ok(if downloaded {
            UpdateStatus::Downloaded { version }
        } else {
            UpdateStatus::Available { version }
        })
    }

    async fn download(&mut self, client: &UpdateClient) {
        let Some((version, asset)) = self.offer.clone() else {
            return;
        };
        if !matches!(self.publisher.status(), UpdateStatus::Available { .. }) {
            return;
        }
        let dest = self.config.store.asset_path(&version, self.asset_name());
        let total = asset.size;
        self.publisher.update(|state| {
            state.status = UpdateStatus::Downloading {
                version: version.clone(),
                received: 0,
                total,
            };
            state.clear_error(UpdateStep::Download);
        });
        let publisher = self.publisher.clone();
        let mut last_percent = 0;
        let progress_version = version.clone();
        let result = client
            .download(&asset, &dest, move |progress| {
                let percent = progress.received * 100 / progress.total.max(1);
                if percent != last_percent {
                    last_percent = percent;
                    publisher.update(|state| {
                        state.status = UpdateStatus::Downloading {
                            version: progress_version.clone(),
                            received: progress.received,
                            total: progress.total,
                        };
                    });
                }
            })
            .await;
        self.publisher.update(|state| match result {
            Ok(()) => state.status = UpdateStatus::Downloaded { version },
            Err(e) => {
                warn!("Downloading the update failed: {e:#}");
                state.status = UpdateStatus::Available { version };
                state.fail(UpdateStep::Download, &e);
            }
        });
    }

    async fn install(&mut self) -> anyhow::Result<PathBuf> {
        let UpdateStatus::Downloaded { version } = self.publisher.status() else {
            bail!("no downloaded update to install");
        };
        let Some((_, asset)) = self.offer.clone() else {
            bail!("no downloaded update to install");
        };
        let archive = self.config.store.asset_path(&version, self.asset_name());
        if !app_update::is_verified(&archive, &asset).await {
            let error = anyhow!("the downloaded update is damaged; download it again");
            let _ = tokio::fs::remove_file(&archive).await;
            self.publisher.update(|state| {
                state.status = UpdateStatus::Available {
                    version: version.clone(),
                };
                state.fail(UpdateStep::Install, &error);
            });
            return Err(error);
        }

        self.publisher.update(|state| {
            state.status = UpdateStatus::Installing {
                version: version.clone(),
            };
            state.clear_error(UpdateStep::Install);
        });
        let record = InstallRecord {
            from_version: self.config.current_version.clone(),
            target_version: version.clone(),
        };
        if let Err(e) = self.config.store.write_install_record(&record) {
            warn!("Could not record the update install: {e:#}");
        }
        match app_update::install(&self.config.installation, &archive).await {
            Ok(()) => Ok(self.config.installation.path.clone()),
            Err(e) => {
                warn!("Installing the update failed: {e:#}");
                self.config.store.remove_install_record();
                self.publisher.update(|state| {
                    state.status = UpdateStatus::Downloaded { version };
                    state.fail(UpdateStep::Install, &e);
                });
                Err(e)
            }
        }
    }
}
