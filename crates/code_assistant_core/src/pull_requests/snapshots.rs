//! Disk snapshots of the last pull request reads, for a cold start: the
//! detail and conversation of the most recently read pull requests and the
//! first page of the most recently read lists. Written off the hot path,
//! one write after the other; read only when memory holds nothing.

use crate::utils::file_utils::atomic_write_unflushed;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;
use tokio::task::JoinHandle;
use tracing::debug;

/// Pull requests (detail and conversation) kept, and lists kept.
const KEPT: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Detail,
    Activity,
    List,
}

impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Kind::Detail => "detail",
            Kind::Activity => "activity",
            Kind::List => "list",
        }
    }
}

/// The key is stored along with the value: the file name is only its hash.
#[derive(Serialize, Deserialize)]
struct Snapshot<K, V> {
    key: K,
    fetched_at: SystemTime,
    value: V,
}

pub(super) struct Snapshots {
    dir: PathBuf,
    /// The last write; the next one waits for it.
    last_write: Mutex<Option<JoinHandle<()>>>,
}

impl Snapshots {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            last_write: Mutex::new(None),
        }
    }

    /// Writes the snapshot in the background, then drops the oldest beyond
    /// what is kept.
    pub fn save<K: Serialize, V: Serialize>(&self, kind: Kind, key: &K, value: &V) {
        let snapshot = Snapshot {
            key,
            fetched_at: SystemTime::now(),
            value,
        };
        let bytes = match serde_json::to_vec(&snapshot) {
            Ok(bytes) => bytes,
            Err(e) => {
                debug!("Cannot encode a pull request snapshot: {e}");
                return;
            }
        };
        let dir = self.dir.clone();
        let path = dir.join(file_name(kind, key));
        let mut last_write = self.last_write.lock().unwrap();
        let previous = last_write.take();
        *last_write = Some(tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let written = tokio::task::spawn_blocking(move || {
                std::fs::create_dir_all(&dir)?;
                atomic_write_unflushed(&path, &bytes)?;
                prune(&dir)
            })
            .await;
            match written {
                Ok(Ok(())) => {}
                Ok(Err(e)) => debug!("Cannot write a pull request snapshot: {e:#}"),
                Err(e) => debug!("Writing a pull request snapshot failed: {e}"),
            }
        }));
    }

    /// The value last saved for `key`, and when it was read from GitHub.
    pub async fn load<K, V>(&self, kind: Kind, key: &K) -> Option<(V, SystemTime)>
    where
        K: Serialize + DeserializeOwned + PartialEq,
        V: DeserializeOwned,
    {
        let path = self.dir.join(file_name(kind, key));
        let bytes = tokio::fs::read(&path).await.ok()?;
        match serde_json::from_slice::<Snapshot<K, V>>(&bytes) {
            Ok(snapshot) if snapshot.key == *key => Some((snapshot.value, snapshot.fetched_at)),
            Ok(_) => None,
            Err(e) => {
                debug!("Ignoring the unreadable snapshot {}: {e}", path.display());
                None
            }
        }
    }

    /// Waits for the writes started so far.
    #[cfg(test)]
    pub async fn flushed(&self) {
        let last_write = self.last_write.lock().unwrap().take();
        if let Some(last_write) = last_write {
            last_write.await.unwrap();
        }
    }
}

/// `<kind>-<hash of the key>.json`; a pull request's detail and conversation
/// share the hash.
fn file_name<K: Serialize>(kind: Kind, key: &K) -> String {
    let encoded = serde_json::to_vec(key).unwrap_or_default();
    let digest = Sha256::digest(&encoded);
    let hash: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{hash}.json", kind.prefix())
}

/// Keeps the [`KEPT`] pull requests and lists written last.
fn prune(dir: &Path) -> anyhow::Result<()> {
    // (group, newest write, files) for pull requests and for lists.
    let mut pull_requests: Vec<(String, SystemTime, Vec<PathBuf>)> = Vec::new();
    let mut lists: Vec<(String, SystemTime, Vec<PathBuf>)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((prefix, hash)) = name
            .strip_suffix(".json")
            .and_then(|stem| stem.split_once('-'))
        else {
            continue;
        };
        let groups = match prefix {
            "detail" | "activity" => &mut pull_requests,
            "list" => &mut lists,
            _ => continue,
        };
        let modified = entry.metadata()?.modified()?;
        match groups.iter_mut().find(|(group, ..)| group == hash) {
            Some((_, newest, files)) => {
                *newest = (*newest).max(modified);
                files.push(entry.path());
            }
            None => groups.push((hash.to_string(), modified, vec![entry.path()])),
        }
    }
    for mut groups in [pull_requests, lists] {
        groups.sort_by_key(|(_, newest, _)| std::cmp::Reverse(*newest));
        for (_, _, files) in groups.into_iter().skip(KEPT) {
            for file in files {
                std::fs::remove_file(file)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_snapshot_reads_back_with_its_time_and_only_for_its_key() {
        let tmp = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::new(tmp.path().join("pull-requests"));
        let before = SystemTime::now();
        snapshots.save(Kind::Detail, &1u64, &"one".to_string());
        snapshots.flushed().await;

        let (value, fetched_at) = snapshots
            .load::<u64, String>(Kind::Detail, &1)
            .await
            .unwrap();
        assert_eq!(value, "one");
        assert!(fetched_at >= before);
        assert!(
            snapshots
                .load::<u64, String>(Kind::Activity, &1)
                .await
                .is_none()
        );
        assert!(
            snapshots
                .load::<u64, String>(Kind::Detail, &2)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn only_the_pull_requests_and_lists_written_last_are_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let snapshots = Snapshots::new(tmp.path().to_path_buf());
        let save = async |kinds: &[Kind], number: u64| {
            for kind in kinds {
                snapshots.save(*kind, &number, &"value");
            }
            snapshots.flushed().await;
            // Distinct modification times, whatever the file system's resolution.
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let kept = |kind, number: u64| tmp.path().join(file_name(kind, &number)).exists();
        let all = [Kind::Detail, Kind::Activity, Kind::List];
        for number in 0..=KEPT as u64 {
            save(&all, number).await;
        }
        assert!(!kept(Kind::Detail, 0) && !kept(Kind::Activity, 0));
        assert!(!kept(Kind::List, 0));
        assert!(kept(Kind::Detail, 1) && kept(Kind::Activity, 1) && kept(Kind::List, 1));

        // Reading the oldest pull request again makes it the newest.
        save(&[Kind::Detail], 1).await;
        save(&[Kind::Detail, Kind::Activity], KEPT as u64 + 1).await;

        assert!(kept(Kind::Detail, 1) && kept(Kind::Activity, 1));
        assert!(!kept(Kind::Detail, 2) && !kept(Kind::Activity, 2));
        let files = std::fs::read_dir(tmp.path()).unwrap().count();
        assert_eq!(files, 3 * KEPT);
    }
}
