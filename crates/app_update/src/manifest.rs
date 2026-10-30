//! The release's `update-manifest.json` (written by
//! `scripts/update-manifest.sh`).

use anyhow::Context;
use semver::Version;
use serde::Deserialize;
use std::collections::BTreeMap;

/// The newest release and its downloads, keyed by file name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Manifest {
    pub version: Version,
    pub assets: BTreeMap<String, Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub url: String,
    /// Lower-case hex.
    pub sha256: String,
    pub size: u64,
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> anyhow::Result<Self> {
        serde_json::from_slice(bytes).context("invalid update manifest")
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_the_release_script_writes() {
        let manifest = Manifest::parse(
            br#"{
              "version": "0.2.19",
              "assets": {
                "code-assistant-linux-x86_64.zip": {
                  "url": "https://example.com/v0.2.19/code-assistant-linux-x86_64.zip",
                  "sha256": "ab12",
                  "size": 42
                }
              }
            }"#,
        )
        .unwrap();

        assert_eq!(manifest.version, Version::new(0, 2, 19));
        let asset = manifest.asset("code-assistant-linux-x86_64.zip").unwrap();
        assert_eq!(asset.size, 42);
        assert_eq!(asset.sha256, "ab12");
        assert!(
            manifest
                .asset("code-assistant-windows-x86_64.zip")
                .is_none()
        );
    }

    #[test]
    fn rejects_a_version_that_is_not_semver() {
        assert!(Manifest::parse(br#"{"version": "latest", "assets": {}}"#).is_err());
    }
}
