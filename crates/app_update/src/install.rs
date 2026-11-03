//! Swapping a downloaded release in for the installation: extract next to
//! it, check what came out, rename it into place.

use crate::installation::{Installation, InstallationKind};
use anyhow::{Context, bail};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Replaces `installation` with the app in `archive`, a zip of the bundle or
/// of the executable. Nothing changes when this fails before the swap; a
/// failed swap is undone.
pub async fn install(installation: &Installation, archive: &Path) -> anyhow::Result<()> {
    let installation = installation.clone();
    let archive = archive.to_owned();
    tokio::task::spawn_blocking(move || install_blocking(&installation, &archive)).await?
}

/// Deletes what an interrupted install left next to the installation, and
/// on Windows the executables earlier installs renamed away.
pub fn remove_leftovers(installation: &Installation) {
    let _ = fs::remove_dir_all(staging_dir(installation));
    let _ = fs::remove_dir_all(backup_path(installation));
    let Some(name) = file_name(&installation.path) else {
        return;
    };
    let Ok(entries) = fs::read_dir(installation.parent()) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(entry_name) = entry.file_name().into_string() else {
            continue;
        };
        if entry_name.starts_with(&format!("{name}.")) && entry_name.ends_with(".old") {
            // Fails while another process still runs it; next start then.
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn install_blocking(installation: &Installation, archive: &Path) -> anyhow::Result<()> {
    let staging = staging_dir(installation);
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).with_context(|| format!("creating {}", staging.display()))?;
    let result = extract(installation, archive, &staging)
        .and_then(|new| check(installation, &new).map(|()| new))
        .and_then(|new| swap(installation, &new));
    let _ = fs::remove_dir_all(&staging);
    if result.is_ok() {
        info!("Installed the update at {}", installation.path.display());
    }
    result
}

fn file_name(path: &Path) -> Option<String> {
    path.file_name()?.to_str().map(str::to_owned)
}

fn hidden_sibling(installation: &Installation, suffix: &str) -> PathBuf {
    let name = file_name(&installation.path).unwrap_or_default();
    installation.parent().join(format!(".{name}.{suffix}"))
}

fn staging_dir(installation: &Installation) -> PathBuf {
    hidden_sibling(installation, "update")
}

fn backup_path(installation: &Installation) -> PathBuf {
    hidden_sibling(installation, "old")
}

/// Unpacks the archive into `staging`; returns the new bundle or executable.
fn extract(installation: &Installation, archive: &Path, staging: &Path) -> anyhow::Result<PathBuf> {
    match installation.kind {
        InstallationKind::MacBundle => extract_bundle(archive, staging),
        InstallationKind::Executable => {
            let name = file_name(&installation.path).context("installation has no file name")?;
            extract_executable(archive, &staging.join(name))
        }
    }
}

#[cfg(target_os = "macos")]
fn extract_bundle(archive: &Path, staging: &Path) -> anyhow::Result<PathBuf> {
    // ditto keeps what a bundle needs and zip crates drop: symlinks,
    // extended attributes, the signature's resource forks.
    run(
        "ditto",
        &[
            "-x".as_ref(),
            "-k".as_ref(),
            archive.as_os_str(),
            staging.as_os_str(),
        ],
    )?;
    let mut bundles = fs::read_dir(staging)?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "app"));
    match (bundles.next(), bundles.next()) {
        (Some(bundle), None) => Ok(bundle),
        _ => bail!("the update does not contain exactly one app bundle"),
    }
}

#[cfg(not(target_os = "macos"))]
fn extract_bundle(_archive: &Path, _staging: &Path) -> anyhow::Result<PathBuf> {
    bail!("app bundles are updated on macOS only")
}

fn extract_executable(archive: &Path, target: &Path) -> anyhow::Result<PathBuf> {
    let mut zip = zip::ZipArchive::new(fs::File::open(archive)?).context("reading the update")?;
    let files: Vec<usize> = (0..zip.len())
        .filter(|&index| zip.by_index(index).is_ok_and(|entry| entry.is_file()))
        .collect();
    let [index] = files[..] else {
        bail!("the update does not contain exactly one file");
    };
    let mut entry = zip.by_index(index)?;
    let mut out = fs::File::create(target)?;
    std::io::copy(&mut entry, &mut out).context("unpacking the update")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(target, fs::Permissions::from_mode(0o755))?;
    }
    Ok(target.to_path_buf())
}

fn check(installation: &Installation, new: &Path) -> anyhow::Result<()> {
    match installation.kind {
        InstallationKind::MacBundle => check_bundle(&installation.path, new),
        InstallationKind::Executable => {
            if fs::metadata(new)?.len() == 0 {
                bail!("the update's executable is empty");
            }
            Ok(())
        }
    }
}

/// The new bundle's signature must be intact, and when the running app is
/// signed by a team, by the same team.
#[cfg(target_os = "macos")]
fn check_bundle(running: &Path, new: &Path) -> anyhow::Result<()> {
    run(
        "codesign",
        &[
            "--verify".as_ref(),
            "--deep".as_ref(),
            "--strict".as_ref(),
            new.as_os_str(),
        ],
    )
    .context("the update's code signature is invalid")?;
    if let Some(team) = signing_team(running) {
        let new_team = signing_team(new);
        if new_team.as_deref() != Some(team.as_str()) {
            bail!(
                "the update is signed by {}, not by this app's team {team}",
                new_team.as_deref().unwrap_or("no team")
            );
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn check_bundle(_running: &Path, _new: &Path) -> anyhow::Result<()> {
    bail!("app bundles are updated on macOS only")
}

#[cfg(target_os = "macos")]
fn signing_team(path: &Path) -> Option<String> {
    let output = std::process::Command::new("codesign")
        .args(["-dv", "--verbose=2"])
        .arg(path)
        .output()
        .ok()?;
    team_identifier(&String::from_utf8_lossy(&output.stderr))
}

/// The `TeamIdentifier` line of `codesign -dv` (which writes to stderr).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn team_identifier(codesign_output: &str) -> Option<String> {
    codesign_output
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|team| !team.is_empty() && *team != "not set")
        .map(str::to_owned)
}

#[cfg(target_os = "macos")]
fn run(program: &str, args: &[&std::ffi::OsStr]) -> anyhow::Result<()> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn swap(installation: &Installation, new: &Path) -> anyhow::Result<()> {
    let path = &installation.path;
    let moved_away = match installation.kind {
        // A directory can not be renamed over another: move the old one
        // aside first.
        InstallationKind::MacBundle => {
            let backup = backup_path(installation);
            let _ = fs::remove_dir_all(&backup);
            Some(backup)
        }
        // A running .exe can not be replaced, but it can be renamed.
        InstallationKind::Executable if cfg!(windows) => Some(old_executable_path(path)),
        // The running process keeps the old inode.
        InstallationKind::Executable => None,
    };
    if let Some(aside) = &moved_away {
        fs::rename(path, aside).with_context(|| format!("moving {} aside", path.display()))?;
    }
    if let Err(e) = fs::rename(new, path) {
        if let Some(aside) = &moved_away
            && let Err(undo) = fs::rename(aside, path)
        {
            warn!("Could not put {} back: {undo}", path.display());
        }
        return Err(e).with_context(|| format!("moving the update to {}", path.display()));
    }
    if installation.kind == InstallationKind::MacBundle
        && let Some(backup) = moved_away
    {
        let _ = fs::remove_dir_all(backup);
    }
    Ok(())
}

/// `<name>.<millis>.old`: unique, since an older one may still be running.
fn old_executable_path(path: &Path) -> PathBuf {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let name = file_name(path).unwrap_or_default();
    path.with_file_name(format!("{name}.{millis}.old"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_files(archive: &Path, files: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(fs::File::create(archive).unwrap());
        for (name, content) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(content).unwrap();
        }
        zip.finish().unwrap();
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn replaces_an_executable() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("code-assistant");
        fs::write(&exe, b"old").unwrap();
        let archive = dir.path().join("update.zip");
        // The archive's file name does not matter; the installed one stays.
        zip_files(&archive, &[("code-assistant.exe", b"new")]);
        let installation = Installation::from_executable(&exe);

        install(&installation, &archive).await.unwrap();

        assert_eq!(fs::read(&exe).unwrap(), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        remove_leftovers(&installation);
        assert_eq!(listing(dir.path()), ["code-assistant", "update.zip"]);
    }

    #[tokio::test]
    async fn refuses_an_archive_with_several_files_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("code-assistant");
        fs::write(&exe, b"old").unwrap();
        let archive = dir.path().join("update.zip");
        zip_files(&archive, &[("a", b"1"), ("b", b"2")]);

        let error = install(&Installation::from_executable(&exe), &archive)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("exactly one file"), "{error:#}");
        assert_eq!(fs::read(&exe).unwrap(), b"old");
        assert_eq!(listing(dir.path()), ["code-assistant", "update.zip"]);
    }

    #[test]
    fn removes_what_an_interrupted_install_left() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("code-assistant.exe");
        fs::write(&exe, b"current").unwrap();
        fs::create_dir(dir.path().join(".code-assistant.exe.update")).unwrap();
        fs::write(dir.path().join("code-assistant.exe.1700000000000.old"), b"").unwrap();
        fs::write(dir.path().join("other.old"), b"").unwrap();

        remove_leftovers(&Installation::from_executable(&exe));

        assert_eq!(listing(dir.path()), ["code-assistant.exe", "other.old"]);
    }

    #[test]
    fn reads_the_team_from_codesign_output() {
        let signed = "Executable=/Applications/Code Assistant.app/Contents/MacOS/code-assistant\n\
                      Authority=Developer ID Application: Someone (VX7AN5CM65)\n\
                      TeamIdentifier=VX7AN5CM65\n";
        assert_eq!(team_identifier(signed).as_deref(), Some("VX7AN5CM65"));
        assert_eq!(
            team_identifier("Signature=adhoc\nTeamIdentifier=not set\n"),
            None
        );
        assert_eq!(team_identifier("code object is not signed at all"), None);
    }

    /// An ad-hoc signed bundle whose resource says `marker`.
    #[cfg(target_os = "macos")]
    fn make_bundle(bundle: &Path, marker: &str) {
        let contents = bundle.join("Contents");
        fs::create_dir_all(contents.join("MacOS")).unwrap();
        fs::create_dir_all(contents.join("Resources")).unwrap();
        fs::write(
            contents.join("Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>test-app</string>
<key>CFBundleIdentifier</key><string>dev.stippi.app-update-test</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>"#,
        )
        .unwrap();
        fs::copy("/usr/bin/true", contents.join("MacOS/test-app")).unwrap();
        fs::write(contents.join("Resources/marker"), marker).unwrap();
        run(
            "codesign",
            &[
                "--force".as_ref(),
                "--deep".as_ref(),
                "-s".as_ref(),
                "-".as_ref(),
                bundle.as_os_str(),
            ],
        )
        .unwrap();
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn replaces_a_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        let running = apps.join("Test App.app");
        make_bundle(&running, "old");
        let release = dir.path().join("release/Test App.app");
        make_bundle(&release, "new");
        let archive = dir.path().join("update.zip");
        run(
            "ditto",
            &[
                "-c".as_ref(),
                "-k".as_ref(),
                "--keepParent".as_ref(),
                release.as_os_str(),
                archive.as_os_str(),
            ],
        )
        .unwrap();
        let installation = Installation::from_executable(&running.join("Contents/MacOS/test-app"));

        install(&installation, &archive).await.unwrap();

        assert_eq!(
            fs::read_to_string(running.join("Contents/Resources/marker")).unwrap(),
            "new"
        );
        assert_eq!(listing(&apps), ["Test App.app"]);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn refuses_a_bundle_with_a_broken_signature() {
        let dir = tempfile::tempdir().unwrap();
        let running = dir.path().join("Applications/Test App.app");
        make_bundle(&running, "old");
        let release = dir.path().join("release/Test App.app");
        make_bundle(&release, "new");
        fs::write(release.join("Contents/Resources/marker"), "tampered").unwrap();
        let archive = dir.path().join("update.zip");
        run(
            "ditto",
            &[
                "-c".as_ref(),
                "-k".as_ref(),
                "--keepParent".as_ref(),
                release.as_os_str(),
                archive.as_os_str(),
            ],
        )
        .unwrap();
        let installation = Installation::from_executable(&running.join("Contents/MacOS/test-app"));

        let error = install(&installation, &archive).await.unwrap_err();

        assert!(error.to_string().contains("signature"), "{error:#}");
        assert_eq!(
            fs::read_to_string(running.join("Contents/Resources/marker")).unwrap(),
            "old"
        );
    }
}
