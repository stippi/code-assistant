//! The updates folder: one subfolder per downloaded version, and the record
//! of the last install.

use anyhow::Context;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::warn;

const INSTALL_RECORD: &str = "install.json";

/// Written right before an install swaps the app: when the next start still
/// runs `from_version`, the update did not apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRecord {
    pub from_version: Version,
    pub target_version: Version,
}

#[derive(Debug, Clone)]
pub struct UpdateStore {
    root: PathBuf,
}

impl UpdateStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the download of `name` for `version` goes.
    pub fn asset_path(&self, version: &Version, name: &str) -> PathBuf {
        self.root.join(version.to_string()).join(name)
    }

    pub fn read_install_record(&self) -> Option<InstallRecord> {
        let text = std::fs::read_to_string(self.root.join(INSTALL_RECORD)).ok()?;
        serde_json::from_str(&text)
            .inspect_err(|e| warn!("Ignoring unreadable update install record: {e}"))
            .ok()
    }

    pub fn write_install_record(&self, record: &InstallRecord) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.root.join(INSTALL_RECORD);
        std::fs::write(&path, serde_json::to_vec(record)?)
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn remove_install_record(&self) {
        let _ = std::fs::remove_file(self.root.join(INSTALL_RECORD));
    }

    /// Deletes the download folders of every version `keep` rejects, and
    /// anything else that is not a version folder.
    pub fn retain(&self, keep: impl Fn(&Version) -> bool) {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let version = entry
                .file_name()
                .to_str()
                .and_then(|name| Version::parse(name).ok());
            if version.as_ref().is_some_and(&keep) {
                continue;
            }
            if let Err(e) = std::fs::remove_dir_all(&path) {
                warn!("Could not remove old update {}: {e}", path.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_record_round_trips_and_goes() {
        let dir = tempfile::tempdir().unwrap();
        let store = UpdateStore::new(dir.path().join("updates"));
        assert_eq!(store.read_install_record(), None);

        let record = InstallRecord {
            from_version: Version::new(0, 2, 18),
            target_version: Version::new(0, 2, 19),
        };
        store.write_install_record(&record).unwrap();
        assert_eq!(store.read_install_record(), Some(record));

        store.remove_install_record();
        assert_eq!(store.read_install_record(), None);
    }

    #[test]
    fn retain_keeps_only_accepted_versions_and_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = UpdateStore::new(dir.path());
        for folder in ["0.2.18", "0.2.19", "0.2.20", "junk"] {
            std::fs::create_dir_all(dir.path().join(folder)).unwrap();
        }
        store
            .write_install_record(&InstallRecord {
                from_version: Version::new(0, 2, 18),
                target_version: Version::new(0, 2, 19),
            })
            .unwrap();

        store.retain(|version| *version > Version::new(0, 2, 18));

        let mut left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["0.2.19", "0.2.20", "install.json"]);
    }
}
