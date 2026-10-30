//! What the frontends show of the self-update.

use app_update::Version;
use chrono::{DateTime, Local};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// This build does not update itself; `reason` says why.
    Disabled {
        reason: String,
    },
    /// Not checked yet.
    Idle,
    Checking,
    UpToDate,
    Available {
        version: Version,
    },
    Downloading {
        version: Version,
        received: u64,
        total: u64,
    },
    Downloaded {
        version: Version,
    },
    Installing {
        version: Version,
    },
}

impl UpdateStatus {
    /// The newer release this status is about, if any.
    pub fn version(&self) -> Option<&Version> {
        match self {
            Self::Available { version }
            | Self::Downloading { version, .. }
            | Self::Downloaded { version }
            | Self::Installing { version } => Some(version),
            _ => None,
        }
    }
}

/// The step an [`UpdateError`] happened in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateStep {
    Check,
    Download,
    Install,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateError {
    pub step: UpdateStep,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateState {
    pub current_version: Version,
    pub status: UpdateStatus,
    /// The last step that failed; cleared when that step succeeds next.
    pub error: Option<UpdateError>,
    pub checked_at: Option<DateTime<Local>>,
    /// The version the last install was meant to bring, when this start
    /// still runs the old one.
    pub not_applied: Option<Version>,
}

impl UpdateState {
    pub fn new(current_version: Version, status: UpdateStatus) -> Self {
        Self {
            current_version,
            status,
            error: None,
            checked_at: None,
            not_applied: None,
        }
    }

    /// Download progress in whole percent while downloading.
    pub fn download_percent(&self) -> Option<u8> {
        match self.status {
            UpdateStatus::Downloading {
                received, total, ..
            } if total > 0 => Some((received.min(total) * 100 / total) as u8),
            UpdateStatus::Downloading { .. } => Some(0),
            _ => None,
        }
    }

    pub(super) fn fail(&mut self, step: UpdateStep, error: &anyhow::Error) {
        self.error = Some(UpdateError {
            step,
            message: format!("{error:#}"),
        });
    }

    pub(super) fn clear_error(&mut self, step: UpdateStep) {
        if self.error.as_ref().is_some_and(|error| error.step == step) {
            self.error = None;
        }
    }
}
