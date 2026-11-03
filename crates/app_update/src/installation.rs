//! Where the running app lives, and whether it can replace itself there.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallationKind {
    /// A macOS `.app` bundle, replaced as a whole.
    MacBundle,
    /// A bare executable.
    Executable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub kind: InstallationKind,
    /// The bundle folder or the executable.
    pub path: PathBuf,
}

impl Installation {
    /// The running app, symlinks resolved (a link in `PATH` is not what an
    /// update replaces).
    pub fn current() -> std::io::Result<Self> {
        let exe = std::env::current_exe()?;
        Ok(Self::from_executable(&exe.canonicalize().unwrap_or(exe)))
    }

    /// The installation an executable belongs to: the bundle when it sits in
    /// `<name>.app/Contents/MacOS/`, else the executable itself.
    pub fn from_executable(exe: &Path) -> Self {
        let bundle = exe
            .parent()
            .filter(|dir| dir.file_name().is_some_and(|name| name == "MacOS"))
            .and_then(Path::parent)
            .filter(|dir| dir.file_name().is_some_and(|name| name == "Contents"))
            .and_then(Path::parent)
            .filter(|dir| dir.extension().is_some_and(|ext| ext == "app"));
        match bundle {
            Some(bundle) => Self {
                kind: InstallationKind::MacBundle,
                path: bundle.to_path_buf(),
            },
            None => Self {
                kind: InstallationKind::Executable,
                path: exe.to_path_buf(),
            },
        }
    }

    /// The folder holding the bundle or executable; staging happens there,
    /// so the swap is a rename on one volume.
    pub fn parent(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    /// Why this installation can not replace itself, if it can not.
    pub fn unsupported_reason(&self) -> Option<String> {
        if is_translocated(&self.path) {
            return Some(
                "macOS runs this copy from a temporary read-only location. \
                 Move the app to the Applications folder to get updates."
                    .to_string(),
            );
        }
        if !is_writable(self.parent()) {
            return Some(format!(
                "{} is not writable, so the app can not update itself there.",
                self.parent().display()
            ));
        }
        None
    }
}

/// Gatekeeper's App Translocation: a quarantined app started where it was
/// downloaded runs from a random read-only mount.
fn is_translocated(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "AppTranslocation")
}

/// Probes by creating a file: permission bits do not tell about ACLs,
/// read-only mounts or SIP.
fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".update-probe-{}", std::process::id()));
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .is_ok();
    if created {
        let _ = std::fs::remove_file(&probe);
    }
    created
}

/// Whether this process is an Intel build running under Rosetta on Apple
/// silicon; its update should be the native build.
pub fn running_under_rosetta() -> bool {
    #[cfg(target_os = "macos")]
    {
        let mut translated: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        // SAFETY: the name is NUL-terminated and `translated`/`size`
        // describe a valid c_int buffer; no new value is written.
        let result = unsafe {
            libc::sysctlbyname(
                c"sysctl.proc_translated".as_ptr(),
                (&mut translated as *mut libc::c_int).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        result == 0 && translated == 1
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_executable_inside_a_bundle_is_the_bundle() {
        let installation = Installation::from_executable(Path::new(
            "/Applications/Code Assistant.app/Contents/MacOS/code-assistant",
        ));
        assert_eq!(installation.kind, InstallationKind::MacBundle);
        assert_eq!(
            installation.path,
            Path::new("/Applications/Code Assistant.app")
        );
        assert_eq!(installation.parent(), Path::new("/Applications"));
    }

    #[test]
    fn a_bare_executable_is_itself() {
        for exe in [
            "/home/me/bin/code-assistant",
            "/tmp/MacOS/code-assistant",
            "/tmp/Contents/MacOS/code-assistant",
        ] {
            let installation = Installation::from_executable(Path::new(exe));
            assert_eq!(installation.kind, InstallationKind::Executable, "{exe}");
            assert_eq!(installation.path, Path::new(exe));
        }
    }

    #[test]
    fn translocated_apps_can_not_update() {
        let installation = Installation::from_executable(Path::new(
            "/private/var/folders/xy/T/AppTranslocation/1234/d/Code Assistant.app/Contents/MacOS/code-assistant",
        ));
        let reason = installation.unsupported_reason().unwrap();
        assert!(reason.contains("Applications"), "{reason}");
    }

    #[test]
    fn needs_a_writable_folder() {
        let dir = tempfile::tempdir().unwrap();
        let installation = Installation::from_executable(&dir.path().join("code-assistant"));
        assert_eq!(installation.unsupported_reason(), None);

        let missing = Installation::from_executable(&dir.path().join("gone/code-assistant"));
        assert!(
            missing
                .unsupported_reason()
                .unwrap()
                .contains("not writable")
        );
    }
}
