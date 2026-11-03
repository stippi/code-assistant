//! Fetching the manifest and downloading an asset: resumed with `Range`
//! after a broken connection, retried with backoff, verified against the
//! manifest's size and sha256.

use crate::manifest::{Asset, Manifest};
use anyhow::{Context, anyhow, bail};
use futures::StreamExt;
use reqwest::StatusCode;
use reqwest::header::RANGE;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

/// How often a download is tried before it fails, and how long to wait
/// before the second attempt (doubling after every further one).
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub attempts: u32,
    pub first_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 5,
            first_delay: Duration::from_secs(1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub received: u64,
    pub total: u64,
}

#[derive(Clone)]
pub struct UpdateClient {
    http: reqwest::Client,
    retry: RetryPolicy,
}

/// Why one download attempt failed: worth another attempt or not.
enum AttemptError {
    Retry(anyhow::Error),
    Fatal(anyhow::Error),
}

impl UpdateClient {
    pub fn new(user_agent: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            retry: RetryPolicy::default(),
        })
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub async fn fetch_manifest(&self, url: &str) -> anyhow::Result<Manifest> {
        let response = self
            .http
            .get(url)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("fetching the update manifest")?;
        let status = response.status();
        if !status.is_success() {
            bail!("fetching the update manifest: HTTP {status}");
        }
        Manifest::parse(&response.bytes().await?)
    }

    /// Downloads `asset` to `dest`, unless a verified copy is there already.
    /// Bytes collect in `<dest>.part`, which survives a failure so the next
    /// call resumes it.
    pub async fn download(
        &self,
        asset: &Asset,
        dest: &Path,
        mut progress: impl FnMut(Progress) + Send,
    ) -> anyhow::Result<()> {
        if is_verified(dest, asset).await {
            return Ok(());
        }
        if let Some(dir) = dest.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let part = part_path(dest);
        let mut delay = self.retry.first_delay;
        let mut last_error = anyhow!("no download attempt");
        for attempt in 1..=self.retry.attempts.max(1) {
            if attempt > 1 {
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            match self.attempt(asset, &part, &mut progress).await {
                Ok(()) if is_verified(&part, asset).await => {
                    tokio::fs::rename(&part, dest).await?;
                    return Ok(());
                }
                Ok(()) => {
                    let _ = tokio::fs::remove_file(&part).await;
                    last_error = anyhow!("the download does not match the release's checksum");
                }
                Err(AttemptError::Fatal(e)) => return Err(e),
                Err(AttemptError::Retry(e)) => last_error = e,
            }
            warn!("Update download attempt {attempt} failed: {last_error:#}");
        }
        Err(last_error)
    }

    /// One request for what `part` lacks; appends to it.
    async fn attempt(
        &self,
        asset: &Asset,
        part: &Path,
        progress: &mut (impl FnMut(Progress) + Send),
    ) -> Result<(), AttemptError> {
        let mut offset = tokio::fs::metadata(part).await.map_or(0, |m| m.len());
        if offset > asset.size {
            let _ = tokio::fs::remove_file(part).await;
            offset = 0;
        }
        if offset == asset.size {
            return Ok(());
        }

        let mut request = self.http.get(&asset.url);
        if offset > 0 {
            request = request.header(RANGE, format!("bytes={offset}-"));
        }
        let response = request
            .send()
            .await
            .map_err(|e| AttemptError::Retry(e.into()))?;
        let status = response.status();
        let append = match status {
            StatusCode::PARTIAL_CONTENT if offset > 0 => true,
            StatusCode::OK => false,
            StatusCode::RANGE_NOT_SATISFIABLE => {
                let _ = tokio::fs::remove_file(part).await;
                return Err(AttemptError::Retry(anyhow!("HTTP {status}")));
            }
            status if is_transient(status) => {
                return Err(AttemptError::Retry(anyhow!("HTTP {status}")));
            }
            status => return Err(AttemptError::Fatal(anyhow!("HTTP {status}"))),
        };
        debug!(append, offset, "Downloading {}", asset.url);

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(part)
            .await
            .map_err(|e| AttemptError::Fatal(e.into()))?;
        let mut received = if append { offset } else { 0 };
        progress(Progress {
            received,
            total: asset.size,
        });
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| AttemptError::Retry(e.into()))?;
            file.write_all(&chunk)
                .await
                .map_err(|e| AttemptError::Fatal(e.into()))?;
            received += chunk.len() as u64;
            if received > asset.size {
                drop(file);
                let _ = tokio::fs::remove_file(part).await;
                return Err(AttemptError::Retry(anyhow!(
                    "the download is larger than the release says"
                )));
            }
            progress(Progress {
                received,
                total: asset.size,
            });
        }
        file.flush()
            .await
            .map_err(|e| AttemptError::Fatal(e.into()))?;
        Ok(())
    }
}

/// Whether `path` holds exactly the asset.
pub async fn is_verified(path: &Path, asset: &Asset) -> bool {
    let path = path.to_owned();
    let asset = asset.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::metadata(&path).is_ok_and(|m| m.len() == asset.size)
            && sha256_hex(&path).is_ok_and(|hash| hash.eq_ignore_ascii_case(&asset.sha256))
    })
    .await
    .unwrap_or(false)
}

fn sha256_hex(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_owned();
    name.push(".part");
    dest.with_file_name(name)
}

fn is_transient(status: StatusCode) -> bool {
    status.is_server_error()
        || status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{HeaderMap, Response, StatusCode as AxumStatus, header};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// What the test server does with the next request.
    #[derive(Clone, Copy)]
    enum Behavior {
        /// Answers, honouring a `Range` request.
        Serve,
        /// Answers with the whole body, ignoring `Range`.
        IgnoreRange,
        /// Sends this many bytes of the answer, then breaks the connection.
        CutAfter(usize),
        Status(u16),
    }

    struct Server {
        body: Vec<u8>,
        script: Mutex<VecDeque<Behavior>>,
        /// The `Range` header of every request.
        ranges: Mutex<Vec<Option<String>>>,
    }

    async fn serve(State(server): State<Arc<Server>>, headers: HeaderMap) -> Response<Body> {
        let range = headers
            .get(header::RANGE)
            .map(|value| value.to_str().unwrap().to_string());
        server.ranges.lock().unwrap().push(range.clone());
        let behavior = server
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Behavior::Serve);
        let offset = range
            .as_deref()
            .and_then(|r| r.strip_prefix("bytes="))
            .and_then(|r| r.strip_suffix('-'))
            .map_or(0, |n| n.parse::<usize>().unwrap());
        let (status, offset) = match behavior {
            Behavior::Status(code) => {
                return Response::builder()
                    .status(code)
                    .body(Body::empty())
                    .unwrap();
            }
            Behavior::IgnoreRange => (AxumStatus::OK, 0),
            _ if offset > 0 => (AxumStatus::PARTIAL_CONTENT, offset),
            _ => (AxumStatus::OK, 0),
        };
        let rest = bytes::Bytes::copy_from_slice(&server.body[offset..]);
        let body = match behavior {
            Behavior::CutAfter(n) => {
                let sent = rest.slice(..n);
                // The break comes a moment later, so hyper sends the bytes
                // before it drops the connection.
                let broken = futures::stream::once(async {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Err(std::io::Error::other("connection lost"))
                });
                Body::from_stream(futures::stream::iter([Ok(sent)]).chain(broken))
            }
            _ => Body::from(rest.clone()),
        };
        Response::builder()
            .status(status)
            .header(header::CONTENT_LENGTH, rest.len())
            .body(body)
            .unwrap()
    }

    struct Fixture {
        server: Arc<Server>,
        asset: Asset,
        dir: tempfile::TempDir,
        client: UpdateClient,
    }

    impl Fixture {
        async fn new(script: impl IntoIterator<Item = Behavior>) -> Self {
            let body: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
            let server = Arc::new(Server {
                body: body.clone(),
                script: Mutex::new(script.into_iter().collect()),
                ranges: Mutex::default(),
            });
            let router = axum::Router::new()
                .route("/asset.zip", axum::routing::get(serve))
                .with_state(server.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

            let asset = Asset {
                url: format!("http://{address}/asset.zip"),
                sha256: Sha256::digest(&body)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
                size: body.len() as u64,
            };
            let client = UpdateClient::new("test").unwrap().with_retry(RetryPolicy {
                attempts: 3,
                first_delay: Duration::ZERO,
            });
            Self {
                server,
                asset,
                dir: tempfile::tempdir().unwrap(),
                client,
            }
        }

        fn dest(&self) -> PathBuf {
            self.dir.path().join("0.2.19").join("asset.zip")
        }

        async fn download(&self) -> anyhow::Result<Vec<Progress>> {
            let mut seen = Vec::new();
            self.client
                .download(&self.asset, &self.dest(), |p| seen.push(p))
                .await
                .map(|()| seen)
        }

        fn ranges(&self) -> Vec<Option<String>> {
            self.server.ranges.lock().unwrap().clone()
        }
    }

    #[tokio::test]
    async fn downloads_and_verifies() {
        let fixture = Fixture::new([]).await;

        let progress = fixture.download().await.unwrap();

        assert_eq!(std::fs::read(fixture.dest()).unwrap(), fixture.server.body);
        assert!(!part_path(&fixture.dest()).exists());
        assert_eq!(progress.last().unwrap().received, fixture.asset.size);
        assert_eq!(fixture.ranges(), [None]);
    }

    #[tokio::test]
    async fn resumes_after_a_broken_connection() {
        let fixture = Fixture::new([Behavior::CutAfter(30_000)]).await;

        fixture.download().await.unwrap();

        assert_eq!(std::fs::read(fixture.dest()).unwrap(), fixture.server.body);
        assert_eq!(
            fixture.ranges(),
            [None, Some("bytes=30000-".to_string())],
            "the second attempt asks only for the rest"
        );
    }

    #[tokio::test]
    async fn starts_over_when_the_server_ignores_the_range() {
        let fixture = Fixture::new([Behavior::CutAfter(30_000), Behavior::IgnoreRange]).await;

        fixture.download().await.unwrap();

        assert_eq!(std::fs::read(fixture.dest()).unwrap(), fixture.server.body);
    }

    #[tokio::test]
    async fn retries_server_errors() {
        let fixture = Fixture::new([Behavior::Status(503), Behavior::Status(429)]).await;

        fixture.download().await.unwrap();

        assert_eq!(fixture.ranges().len(), 3);
    }

    #[tokio::test]
    async fn gives_up_at_once_on_a_missing_asset() {
        let fixture = Fixture::new([Behavior::Status(404)]).await;

        let error = fixture.download().await.unwrap_err();

        assert!(error.to_string().contains("404"), "{error:#}");
        assert_eq!(fixture.ranges().len(), 1);
    }

    #[tokio::test]
    async fn keeps_the_part_when_attempts_run_out_and_resumes_it_later() {
        let fixture = Fixture::new([
            Behavior::CutAfter(10_000),
            Behavior::Status(502),
            Behavior::Status(502),
        ])
        .await;

        assert!(fixture.download().await.is_err());
        assert_eq!(
            std::fs::metadata(part_path(&fixture.dest())).unwrap().len(),
            10_000
        );

        fixture.download().await.unwrap();
        assert_eq!(
            fixture.ranges().last().unwrap().as_deref(),
            Some("bytes=10000-")
        );
        assert_eq!(std::fs::read(fixture.dest()).unwrap(), fixture.server.body);
    }

    #[tokio::test]
    async fn rejects_a_download_with_the_wrong_checksum() {
        let mut fixture = Fixture::new([]).await;
        fixture.asset.sha256 = "00".repeat(32);

        let error = fixture.download().await.unwrap_err();

        assert!(error.to_string().contains("checksum"), "{error:#}");
        assert!(!fixture.dest().exists());
        assert!(!part_path(&fixture.dest()).exists());
        assert_eq!(fixture.ranges().len(), 3);
    }

    #[tokio::test]
    async fn reuses_a_verified_download() {
        let fixture = Fixture::new([]).await;
        fixture.download().await.unwrap();

        fixture.download().await.unwrap();

        assert_eq!(fixture.ranges().len(), 1);
    }

    #[tokio::test]
    async fn fetches_the_manifest() {
        let app = axum::Router::new().route(
            "/update-manifest.json",
            axum::routing::get(|| async { r#"{"version": "1.2.3", "assets": {}}"# }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = UpdateClient::new("test").unwrap();

        let manifest = client
            .fetch_manifest(&format!("http://{address}/update-manifest.json"))
            .await
            .unwrap();
        assert_eq!(manifest.version, semver::Version::new(1, 2, 3));

        let missing = client
            .fetch_manifest(&format!("http://{address}/missing.json"))
            .await;
        assert!(missing.unwrap_err().to_string().contains("404"));
    }
}
