//! Copy-on-write copies of a working tree.
//!
//! A sub-agent's workspace starts as a copy of everything in the parent's
//! tree, ignored files included: build outputs, installed dependencies and
//! local configuration come along, so the sub-agent can build and test right
//! away without a setup step. The copy shares the file system blocks with
//! the original until either side changes a file, which needs a file system
//! that can clone files (APFS on macOS; btrfs or XFS on Linux). Elsewhere
//! [`clone_tree`] fails and the caller falls back to a plain checkout.

use std::io;
use std::path::{Path, PathBuf};

/// Clone every entry of `source` into the existing directory `target`,
/// except `source/.git` and the paths in `skip` (absolute, as `source` is).
/// Symbolic links are copied as links; files keep their permissions and
/// modification times, so build tools see them unchanged.
pub(crate) async fn clone_tree(source: &Path, target: &Path, skip: &[PathBuf]) -> io::Result<()> {
    let (source, target) = (source.to_path_buf(), target.to_path_buf());
    let mut skip = skip.to_vec();
    skip.push(source.join(".git"));
    tokio::task::spawn_blocking(move || clone_entries(&source, &target, &skip))
        .await
        .map_err(io::Error::other)?
}

fn clone_entries(source: &Path, target: &Path, skip: &[PathBuf]) -> io::Result<()> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        if skip.contains(&path) {
            continue;
        }
        let destination = target.join(entry.file_name());
        let file_type = entry.file_type()?;
        // A directory holding something to skip is copied entry by entry;
        // any other one in one go.
        if file_type.is_dir() && skip.iter().any(|s| s.starts_with(&path)) {
            std::fs::create_dir(&destination)?;
            clone_entries(&path, &destination, skip)?;
        } else {
            clone_one(&path, &destination)?;
        }
    }
    Ok(())
}

/// Clone a file, a symbolic link or a whole directory tree.
#[cfg(target_vendor = "apple")]
fn clone_one(source: &Path, target: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let to_c = |path: &Path| {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    };
    // `CLONE_NOFOLLOW` from <sys/clonefile.h>: clone a symbolic link itself.
    const CLONE_NOFOLLOW: u32 = 0x0001;
    let (src, dst) = (to_c(source)?, to_c(target)?);
    // clonefile(2) copies a directory with everything below it in one call.
    // SAFETY: both arguments are valid NUL-terminated paths.
    let result = unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), CLONE_NOFOLLOW) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Clone a file, a symbolic link or a whole directory tree.
#[cfg(target_os = "linux")]
fn clone_one(source: &Path, target: &Path) -> io::Result<()> {
    let output = std::process::Command::new("cp")
        .args([
            "-R",
            "--reflink=always",
            "--no-dereference",
            "--preserve=mode,timestamps,links",
            "--",
        ])
        .arg(source)
        .arg(target)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "cp failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Clone a file, a symbolic link or a whole directory tree.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
fn clone_one(_source: &Path, _target: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "copy-on-write copies are not supported on this platform",
    ))
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn everything_but_git_and_skipped_paths_is_copied() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        std::fs::create_dir_all(source.join(".git")).unwrap();
        std::fs::create_dir_all(source.join("target/debug")).unwrap();
        std::fs::create_dir_all(source.join("nested/other-worktree")).unwrap();
        std::fs::create_dir_all(source.join("nested/kept")).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(source.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(source.join("target/debug/app"), "binary").unwrap();
        std::fs::write(source.join("nested/kept/a.txt"), "a").unwrap();
        std::fs::write(source.join("nested/other-worktree/b.txt"), "b").unwrap();
        std::os::unix::fs::symlink("kept/a.txt", source.join("nested/link")).unwrap();

        clone_tree(&source, &target, &[source.join("nested/other-worktree")])
            .await
            .unwrap();

        assert!(!target.join(".git").exists());
        assert!(!target.join("nested/other-worktree").exists());
        assert_eq!(
            std::fs::read_to_string(target.join("target/debug/app")).unwrap(),
            "binary"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("nested/kept/a.txt")).unwrap(),
            "a"
        );
        assert_eq!(
            std::fs::read_link(target.join("nested/link")).unwrap(),
            Path::new("kept/a.txt")
        );
        // The copy is independent of the original.
        std::fs::write(target.join("nested/kept/a.txt"), "changed").unwrap();
        assert_eq!(
            std::fs::read_to_string(source.join("nested/kept/a.txt")).unwrap(),
            "a"
        );
    }
}
