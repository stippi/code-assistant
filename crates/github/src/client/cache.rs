//! The client's small caches: validators of REST reads and pull request
//! node ids.

use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::Duration;
use tokio::time::Instant;

use crate::transport::Headers;

const ETAG_CAPACITY: usize = 512;
const ETAG_TTL: Duration = Duration::from_secs(60 * 60);
const NODE_ID_CAPACITY: usize = 128;

/// The last answer to a REST read and its validator.
#[derive(Debug, Clone)]
pub(crate) struct Validated {
    pub etag: String,
    pub headers: Headers,
    pub body: Vec<u8>,
    stored: Instant,
}

/// Answers by credential, host and path, so an unchanged read comes back as
/// a 304, which GitHub does not charge. A 304 renews the entry, so a read
/// still being polled outlives the hour.
pub(crate) struct EtagCache {
    entries: Mutex<LruCache<String, Validated>>,
}

impl EtagCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(LruCache::new(NonZeroUsize::new(ETAG_CAPACITY).unwrap())),
        }
    }

    pub fn get(&self, key: &str) -> Option<Validated> {
        let mut entries = self.entries.lock().unwrap();
        match entries.get(key) {
            Some(entry) if entry.stored.elapsed() < ETAG_TTL => Some(entry.clone()),
            Some(_) => {
                entries.pop(key);
                None
            }
            None => None,
        }
    }

    pub fn renew(&self, key: &str) {
        if let Some(entry) = self.entries.lock().unwrap().get_mut(key) {
            entry.stored = Instant::now();
        }
    }

    pub fn store(&self, key: String, etag: Option<&str>, headers: &Headers, body: &[u8]) {
        let mut entries = self.entries.lock().unwrap();
        match etag.map(str::trim).filter(|e| !e.is_empty()) {
            Some(etag) => {
                entries.put(
                    key,
                    Validated {
                        etag: etag.to_string(),
                        headers: headers.clone(),
                        body: body.to_vec(),
                        stored: Instant::now(),
                    },
                );
            }
            None => {
                entries.pop(&key);
            }
        }
    }
}

/// Pull request node ids never change, so mutations look each up once.
pub(crate) struct NodeIdCache {
    ids: Mutex<LruCache<String, String>>,
}

impl NodeIdCache {
    pub fn new() -> Self {
        Self {
            ids: Mutex::new(LruCache::new(NonZeroUsize::new(NODE_ID_CAPACITY).unwrap())),
        }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.ids.lock().unwrap().get(key).cloned()
    }

    pub fn put(&self, key: String, id: String) {
        self.ids.lock().unwrap().put(key, id);
    }
}
