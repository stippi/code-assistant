use super::*;
use crate::session::event_stream::{EventPayload, EventStream, Subscription};
use crate::ui::UiEvent;
use app_update::{InstallRecord, Installation, RetryPolicy, UpdateStore};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ASSET: &str = "code-assistant-test.zip";
const WAIT: Duration = Duration::from_secs(10);

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

/// What the fake release page serves.
#[derive(Default)]
struct Release {
    /// `None` answers the manifest with a server error.
    manifest: Mutex<Option<String>>,
    /// `None` answers the asset with 404.
    asset: Mutex<Option<Vec<u8>>>,
    asset_requests: AtomicUsize,
}

async fn manifest(State(release): State<Arc<Release>>) -> Result<String, StatusCode> {
    release
        .manifest
        .lock()
        .unwrap()
        .clone()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)
}

async fn asset(
    State(release): State<Arc<Release>>,
    UrlPath(_name): UrlPath<String>,
) -> Result<Vec<u8>, StatusCode> {
    release.asset_requests.fetch_add(1, Ordering::SeqCst);
    release
        .asset
        .lock()
        .unwrap()
        .clone()
        .ok_or(StatusCode::NOT_FOUND)
}

fn zip_of(content: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("code-assistant", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(content).unwrap();
    zip.finish().unwrap().into_inner()
}

struct Fixture {
    dir: tempfile::TempDir,
    base: String,
    release: Arc<Release>,
}

impl Fixture {
    async fn new() -> Self {
        let release = Arc::new(Release::default());
        let router = axum::Router::new()
            .route("/update-manifest.json", axum::routing::get(manifest))
            .route("/download/:name", axum::routing::get(asset))
            .with_state(release.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(dir.path().join("bin/code-assistant"), b"0.2.18").unwrap();
        Self { dir, base, release }
    }

    fn exe(&self) -> PathBuf {
        self.dir.path().join("bin/code-assistant")
    }

    fn store(&self) -> UpdateStore {
        UpdateStore::new(self.dir.path().join("updates"))
    }

    /// Publishes `version` with an executable saying `version`.
    fn publish(&self, version: &str) {
        let archive = zip_of(version.as_bytes());
        let sha256: String = Sha256::digest(&archive)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        *self.release.manifest.lock().unwrap() = Some(
            serde_json::json!({
                "version": version,
                "assets": {
                    ASSET: {
                        "url": format!("{}/download/{ASSET}", self.base),
                        "sha256": sha256,
                        "size": archive.len(),
                    }
                }
            })
            .to_string(),
        );
        *self.release.asset.lock().unwrap() = Some(archive);
    }

    fn config(&self) -> UpdateConfig {
        UpdateConfig {
            feed: Some(format!("{}/update-manifest.json", self.base)),
            disabled_by_env: false,
            current_version: v("0.2.18"),
            installation: Installation::from_executable(&self.exe()),
            asset_name: Some(ASSET.to_string()),
            store: self.store(),
            retry: RetryPolicy {
                attempts: 2,
                first_delay: Duration::ZERO,
            },
            first_check: Duration::from_secs(3600),
            check_interval: Duration::from_secs(3600),
        }
    }

    fn start_with(&self, config: UpdateConfig) -> (UpdateService, Subscription) {
        let events = EventStream::new();
        let subscription = events.subscribe();
        let (service, worker) = UpdateService::new(config, events);
        tokio::spawn(worker);
        (service, subscription)
    }

    fn start(&self) -> (UpdateService, Subscription) {
        self.start_with(self.config())
    }
}

async fn wait_for_state(
    events: &mut Subscription,
    pred: impl Fn(&UpdateState) -> bool,
) -> UpdateState {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(event) = events.recv().await
                && let EventPayload::Ui(UiEvent::UpdateStateChanged { state }) = event.payload
                && pred(&state)
            {
                return state;
            }
        }
    })
    .await
    .expect("update state")
}

#[test]
fn picks_the_asset_of_the_platform_and_packaging() {
    use InstallationKind::{Executable, MacBundle};
    let cases = [
        (
            "aarch64-apple-darwin",
            MacBundle,
            false,
            "Code-Assistant-macos-aarch64.app.zip",
        ),
        (
            "x86_64-apple-darwin",
            MacBundle,
            false,
            "Code-Assistant-macos-x86_64.app.zip",
        ),
        (
            "x86_64-apple-darwin",
            MacBundle,
            true,
            "Code-Assistant-macos-aarch64.app.zip",
        ),
        (
            "aarch64-apple-darwin",
            Executable,
            false,
            "code-assistant-macos-aarch64.zip",
        ),
        (
            "x86_64-unknown-linux-gnu",
            Executable,
            false,
            "code-assistant-linux-x86_64.zip",
        ),
        (
            "x86_64-pc-windows-msvc",
            Executable,
            false,
            "code-assistant-windows-x86_64.zip",
        ),
    ];
    for (target, kind, rosetta, expected) in cases {
        assert_eq!(asset_name(target, kind, rosetta).as_deref(), Some(expected));
    }
    assert_eq!(
        asset_name("aarch64-unknown-linux-gnu", Executable, false),
        None
    );
}

#[tokio::test]
async fn without_a_feed_the_build_never_updates() {
    let fixture = Fixture::new().await;
    fixture.publish("0.2.19");
    let (service, _events) = fixture.start_with(UpdateConfig {
        feed: None,
        ..fixture.config()
    });

    service.check().await;

    assert!(matches!(
        service.state().status,
        UpdateStatus::Disabled { .. }
    ));
    assert!(service.install().await.is_err());
    assert_eq!(std::fs::read(fixture.exe()).unwrap(), b"0.2.18");
}

#[tokio::test]
async fn finds_downloads_and_installs_a_newer_release() {
    let fixture = Fixture::new().await;
    fixture.publish("0.2.19");
    let (service, mut events) = fixture.start();

    service.check().await;
    let state = service.state();
    assert_eq!(
        state.status,
        UpdateStatus::Available {
            version: v("0.2.19")
        }
    );
    assert!(state.checked_at.is_some());

    service.download().await;
    wait_for_state(&mut events, |state| {
        matches!(state.status, UpdateStatus::Downloading { .. })
    })
    .await;
    assert_eq!(
        service.state().status,
        UpdateStatus::Downloaded {
            version: v("0.2.19")
        }
    );

    let launch = service.install().await.unwrap();

    assert_eq!(launch, fixture.exe());
    assert_eq!(std::fs::read(fixture.exe()).unwrap(), b"0.2.19");
    assert_eq!(
        fixture.store().read_install_record(),
        Some(InstallRecord {
            from_version: v("0.2.18"),
            target_version: v("0.2.19"),
        })
    );
}

#[tokio::test]
async fn is_up_to_date_when_the_release_is_not_newer() {
    let fixture = Fixture::new().await;
    fixture.publish("0.2.18");
    let (service, _events) = fixture.start();

    service.check().await;

    assert_eq!(service.state().status, UpdateStatus::UpToDate);
}

#[tokio::test]
async fn a_failed_check_keeps_what_was_found_and_records_the_error() {
    let fixture = Fixture::new().await;
    fixture.publish("0.2.19");
    let (service, _events) = fixture.start();
    service.check().await;

    *fixture.release.manifest.lock().unwrap() = None;
    service.check().await;

    let state = service.state();
    assert_eq!(
        state.status,
        UpdateStatus::Available {
            version: v("0.2.19")
        }
    );
    assert_eq!(state.error.unwrap().step, UpdateStep::Check);
}

#[tokio::test]
async fn a_failed_download_offers_the_download_again() {
    let fixture = Fixture::new().await;
    fixture.publish("0.2.19");
    *fixture.release.asset.lock().unwrap() = None;
    let (service, _events) = fixture.start();
    service.check().await;

    service.download().await;

    let state = service.state();
    assert_eq!(
        state.status,
        UpdateStatus::Available {
            version: v("0.2.19")
        }
    );
    let error = state.error.unwrap();
    assert_eq!(error.step, UpdateStep::Download);
    assert!(error.message.contains("404"), "{}", error.message);

    fixture.publish("0.2.19");
    service.download().await;
    let state = service.state();
    assert_eq!(
        state.status,
        UpdateStatus::Downloaded {
            version: v("0.2.19")
        }
    );
    assert_eq!(state.error, None);
}

#[tokio::test]
async fn a_later_start_finds_the_earlier_download() {
    let fixture = Fixture::new().await;
    fixture.publish("0.2.19");
    let (service, _events) = fixture.start();
    service.check().await;
    service.download().await;
    let requests = fixture.release.asset_requests.load(Ordering::SeqCst);

    let (restarted, _events) = fixture.start();
    restarted.check().await;

    assert_eq!(
        restarted.state().status,
        UpdateStatus::Downloaded {
            version: v("0.2.19")
        }
    );
    assert_eq!(
        fixture.release.asset_requests.load(Ordering::SeqCst),
        requests
    );
}

#[tokio::test]
async fn a_newer_release_replaces_an_older_download() {
    let fixture = Fixture::new().await;
    fixture.publish("0.2.19");
    let (service, _events) = fixture.start();
    service.check().await;
    service.download().await;

    fixture.publish("0.2.20");
    service.check().await;

    assert_eq!(
        service.state().status,
        UpdateStatus::Available {
            version: v("0.2.20")
        }
    );
    assert!(!fixture.dir.path().join("updates/0.2.19").exists());
}

#[tokio::test]
async fn notices_an_install_that_did_not_apply_and_cleans_up() {
    let fixture = Fixture::new().await;
    let store = fixture.store();
    for version in ["0.2.17", "0.2.18", "0.2.19"] {
        std::fs::create_dir_all(store.root().join(version)).unwrap();
    }
    store
        .write_install_record(&InstallRecord {
            from_version: v("0.2.18"),
            target_version: v("0.2.19"),
        })
        .unwrap();

    let (service, _events) = fixture.start();

    assert_eq!(service.state().not_applied, Some(v("0.2.19")));
    assert!(!store.root().join("0.2.17").exists());
    assert!(!store.root().join("0.2.18").exists());
    assert!(store.root().join("0.2.19").exists());
}

#[tokio::test]
async fn forgets_the_record_of_an_install_that_applied() {
    let fixture = Fixture::new().await;
    let store = fixture.store();
    store
        .write_install_record(&InstallRecord {
            from_version: v("0.2.17"),
            target_version: v("0.2.18"),
        })
        .unwrap();

    let (service, _events) = fixture.start();

    assert_eq!(service.state().not_applied, None);
    assert_eq!(store.read_install_record(), None);
}

#[tokio::test]
async fn checks_on_its_own() {
    let fixture = Fixture::new().await;
    fixture.publish("0.2.19");

    let (_service, mut events) = fixture.start_with(UpdateConfig {
        first_check: Duration::from_millis(10),
        ..fixture.config()
    });

    wait_for_state(&mut events, |state| {
        matches!(state.status, UpdateStatus::Available { .. })
    })
    .await;
}
