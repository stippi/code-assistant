//! Large tool results, stored next to the session record instead of in it.
//!
//! A tool execution's `result_json` that serializes to more than
//! [`BLOB_THRESHOLD`] bytes is written to `blobs/<sha256>.json` in the
//! session folder and replaced in the record by a reference:
//!
//! ```json
//! {"$blob": "9f86d081…", "size": 18392011}
//! ```
//!
//! Blobs are content-addressed: one is written once and never changed, so
//! saving the session again doesn't rewrite it, and identical results (the
//! same screenshot, a re-recorded execution) share a file.

use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::path::PathBuf;

use crate::utils::file_utils::atomic_write;

/// Results larger than this move out of the session record. Small results
/// stay inline so the record remains readable.
pub const BLOB_THRESHOLD: usize = 4 * 1024;

const BLOB_KEY: &str = "$blob";
const SIZE_KEY: &str = "size";

/// The blobs of one session.
pub struct BlobStore {
    dir: PathBuf,
}

impl BlobStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The value to store in the record: `value` itself when it is small,
    /// otherwise a reference to its blob, which is written unless it
    /// already exists.
    pub fn externalize(&self, value: Value) -> Result<Value> {
        let bytes = serde_json::to_vec(&value)?;
        if bytes.len() <= BLOB_THRESHOLD {
            return Ok(value);
        }
        let hash =
            Sha256::digest(&bytes)
                .iter()
                .fold(String::with_capacity(64), |mut hex, byte| {
                    let _ = write!(hex, "{byte:02x}");
                    hex
                });
        let path = self.path(&hash);
        if !path.exists() {
            atomic_write(&path, &bytes)?;
        }
        Ok(serde_json::json!({ BLOB_KEY: hash, SIZE_KEY: bytes.len() }))
    }

    /// The value a record entry stands for: the blob's content when it is a
    /// reference, otherwise the entry itself.
    pub fn resolve(&self, value: Value) -> Result<Value> {
        let Some(hash) = blob_reference(&value) else {
            return Ok(value);
        };
        let path = self.path(hash);
        let bytes =
            std::fs::read(&path).with_context(|| format!("missing blob {}", path.display()))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    fn path(&self, hash: &str) -> PathBuf {
        self.dir.join(format!("{hash}.json"))
    }
}

/// The hash of a blob reference: an object of exactly `$blob` (a hex hash)
/// and `size`.
fn blob_reference(value: &Value) -> Option<&str> {
    let object = value.as_object()?;
    if object.len() != 2 || !object.contains_key(SIZE_KEY) {
        return None;
    }
    let hash = object.get(BLOB_KEY)?.as_str()?;
    (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn large(text: &str) -> Value {
        json!({ "output": text.repeat(BLOB_THRESHOLD) })
    }

    fn blob_files(store: &BlobStore) -> Vec<PathBuf> {
        std::fs::read_dir(&store.dir)
            .map(|entries| entries.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn small_values_stay_inline() {
        let dir = tempdir().unwrap();
        let store = BlobStore::new(dir.path().join("blobs"));
        let value = json!({ "output": "short" });

        assert_eq!(store.externalize(value.clone()).unwrap(), value);
        assert!(blob_files(&store).is_empty());
    }

    #[test]
    fn large_values_round_trip_through_a_blob() {
        let dir = tempdir().unwrap();
        let store = BlobStore::new(dir.path().join("blobs"));
        let value = large("x");

        let stored = store.externalize(value.clone()).unwrap();
        assert!(blob_reference(&stored).is_some());
        assert_eq!(
            stored["size"],
            json!(serde_json::to_vec(&value).unwrap().len())
        );
        assert_eq!(store.resolve(stored).unwrap(), value);
    }

    #[test]
    fn identical_values_share_one_blob_that_is_never_rewritten() {
        let dir = tempdir().unwrap();
        let store = BlobStore::new(dir.path().join("blobs"));

        let first = store.externalize(large("x")).unwrap();
        let [path] = blob_files(&store).try_into().unwrap();
        // Mark the file: an existing blob must not be written again.
        std::fs::write(&path, br#""marker""#).unwrap();

        assert_eq!(store.externalize(large("x")).unwrap(), first);
        assert_eq!(blob_files(&store), std::slice::from_ref(&path));
        assert_eq!(std::fs::read(&path).unwrap(), br#""marker""#);

        store.externalize(large("y")).unwrap();
        assert_eq!(blob_files(&store).len(), 2);
    }

    #[test]
    fn values_that_merely_look_like_references_are_kept() {
        let dir = tempdir().unwrap();
        let store = BlobStore::new(dir.path().join("blobs"));
        for value in [
            json!({ "$blob": "not-a-hash", "size": 1 }),
            json!({ "$blob": "a".repeat(64) }),
            json!({ "$blob": "a".repeat(64), "size": 1, "other": true }),
        ] {
            assert_eq!(store.resolve(value.clone()).unwrap(), value);
        }
    }

    #[test]
    fn a_missing_blob_is_an_error() {
        let dir = tempdir().unwrap();
        let store = BlobStore::new(dir.path().join("blobs"));
        let reference = json!({ "$blob": "a".repeat(64), "size": 1 });
        assert!(store.resolve(reference).is_err());
    }
}
