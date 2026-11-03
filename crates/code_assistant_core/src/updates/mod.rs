//! Self-update: notices a newer release, downloads it when the user asks,
//! swaps it in on *Restart to update*. The mechanics are the `app_update`
//! crate's; this module decides when to check, which asset this build
//! needs, and keeps the state the title bar shows. See
//! `docs/self-update.md`.

mod service;
mod state;
#[cfg(test)]
mod tests;

pub use app_update::{InstallationKind, Version};
pub use service::{UpdateConfig, UpdateService};
pub use state::{UpdateError, UpdateState, UpdateStatus, UpdateStep};

/// The release asset that updates a build for `target` installed as
/// `kind`; `None` for platforms the release does not build. An Intel build
/// under Rosetta gets the native one.
pub fn asset_name(target: &str, kind: InstallationKind, rosetta: bool) -> Option<String> {
    let platform = match target {
        "aarch64-apple-darwin" => "macos-aarch64",
        "x86_64-apple-darwin" if rosetta => "macos-aarch64",
        "x86_64-apple-darwin" => "macos-x86_64",
        "x86_64-unknown-linux-gnu" => "linux-x86_64",
        "x86_64-pc-windows-msvc" => "windows-x86_64",
        _ => return None,
    };
    Some(match kind {
        InstallationKind::MacBundle => format!("Code-Assistant-{platform}.app.zip"),
        InstallationKind::Executable => format!("code-assistant-{platform}.zip"),
    })
}
