//! Self-update mechanics for an app shipped as zips on a release page: the
//! update manifest, where the running installation lives, downloading an
//! asset (resumed, retried, verified), swapping it in and cleaning up after.
//!
//! When to check, what to download and the state users see belong to the
//! app (`code_assistant_core::updates`); see `docs/self-update.md`.

mod client;
mod install;
mod installation;
mod manifest;
mod store;

pub use client::{Progress, RetryPolicy, UpdateClient, is_verified};
pub use install::{install, remove_leftovers};
pub use installation::{Installation, InstallationKind, running_under_rosetta};
pub use manifest::{Asset, Manifest};
pub use semver::Version;
pub use store::{InstallRecord, UpdateStore};
