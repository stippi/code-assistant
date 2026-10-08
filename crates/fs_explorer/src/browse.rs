//! Read-only browsing of a project for a file viewer: one directory level at
//! a time, a single file's text, and the flat list of files for a name
//! filter. Every path is relative to a root and may not leave it.

use crate::encoding::{read_file_with_encoding, write_file_with_encoding};
use anyhow::{Result, anyhow, bail};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// Bytes read at most for one file; larger files are reported as too large.
pub const MAX_VIEW_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Entries returned at most for one directory.
pub const MAX_DIR_ENTRIES: usize = 5_000;

/// Files returned at most by [`list_files`].
pub const MAX_LISTED_FILES: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    Dir,
    File,
}

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    /// Excluded by gitignore (or the `.git` directory itself).
    pub ignored: bool,
}

/// One directory level: directories first, then files, each by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirListing {
    pub entries: Vec<DirEntry>,
    /// Entries left out beyond [`MAX_DIR_ENTRIES`].
    pub omitted: usize,
}

/// A file's content as far as a viewer can show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    Text(String),
    Binary,
    TooLarge { size: u64 },
}

/// Resolve `rel` under `root`, refusing absolute paths, `..`, and symlinks
/// that lead outside the root.
pub fn resolve_inside(root: &Path, rel: &Path) -> Result<PathBuf> {
    for component in rel.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => bail!("Path {} is not inside the project", rel.display()),
        }
    }
    let path = root.join(rel);
    let canonical_root = root.canonicalize()?;
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(&canonical_root) {
        bail!("Path {} leads outside the project", rel.display());
    }
    Ok(path)
}

/// List the directory `rel` under `root` (`""` for the root itself).
pub fn list_dir(root: &Path, rel: &Path) -> Result<DirListing> {
    let dir = resolve_inside(root, rel)?;
    if !dir.is_dir() {
        bail!("{} is not a directory", rel.display());
    }

    // The walker yields what gitignore keeps; everything else is ignored.
    let kept: HashSet<OsString> = WalkBuilder::new(&dir)
        .max_depth(Some(1))
        .hidden(false)
        .require_git(false)
        .build()
        .flatten()
        .filter(|entry| entry.depth() == 1)
        .map(|entry| entry.file_name().to_owned())
        .collect();

    let mut entries: Vec<DirEntry> = std::fs::read_dir(&dir)?
        .flatten()
        .map(|entry| {
            let name = entry.file_name();
            // `metadata` follows symlinks, so a link to a directory is one.
            let is_dir = entry.path().metadata().is_ok_and(|m| m.is_dir());
            DirEntry {
                ignored: name == ".git" || !kept.contains(&name),
                name: name.to_string_lossy().into_owned(),
                kind: if is_dir {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                },
            }
        })
        .collect();
    entries.sort_by(|a, b| {
        (a.kind != EntryKind::Dir)
            .cmp(&(b.kind != EntryKind::Dir))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    let omitted = entries.len().saturating_sub(MAX_DIR_ENTRIES);
    entries.truncate(MAX_DIR_ENTRIES);
    Ok(DirListing { entries, omitted })
}

/// Read the file `rel` under `root` for display.
pub fn read_file(root: &Path, rel: &Path) -> Result<FileContent> {
    let path = resolve_inside(root, rel)?;
    let metadata = path.metadata()?;
    if !metadata.is_file() {
        bail!("{} is not a file", rel.display());
    }
    if metadata.len() > MAX_VIEW_FILE_BYTES {
        return Ok(FileContent::TooLarge {
            size: metadata.len(),
        });
    }
    let bytes = std::fs::read(&path)?;
    let sample = &bytes[..bytes.len().min(8 * 1024)];
    if content_inspector::inspect(sample).is_binary() {
        return Ok(FileContent::Binary);
    }
    let (text, _) = read_file_with_encoding(&path)?;
    Ok(FileContent::Text(text))
}

/// Replace the content of the existing file `rel` under `root`, keeping the
/// encoding it has on disk.
pub fn write_file(root: &Path, rel: &Path, text: &str) -> Result<()> {
    let path = resolve_inside(root, rel)?;
    if !path.is_file() {
        bail!("{} is not a file", rel.display());
    }
    let (_, encoding) = read_file_with_encoding(&path)?;
    write_file_with_encoding(&path, text, &encoding)
}

/// Every file under `root` that gitignore keeps, as `/`-separated relative
/// paths in walk order, at most [`MAX_LISTED_FILES`]. The flag tells whether
/// the list was cut.
pub fn list_files(root: &Path) -> Result<(Vec<String>, bool)> {
    if !root.is_dir() {
        return Err(anyhow!("{} is not a directory", root.display()));
    }
    let mut files = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if files.len() == MAX_LISTED_FILES {
            return Ok((files, true));
        }
        if let Ok(rel) = entry.path().strip_prefix(root) {
            let parts: Vec<_> = rel.iter().map(|p| p.to_string_lossy()).collect();
            files.push(parts.join("/"));
        }
    }
    Ok((files, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "target\n*.log\n").unwrap();
        fs::write(root.join("README.md"), "hello\n").unwrap();
        fs::write(root.join("app.log"), "noise\n").unwrap();
        fs::write(root.join("src/lib.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src/nested/b.rs"), "").unwrap();
        fs::write(root.join("target/debug/out"), "").unwrap();
        dir
    }

    fn names(listing: &DirListing) -> Vec<(&str, bool)> {
        listing
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.ignored))
            .collect()
    }

    #[test]
    fn lists_directories_first_and_marks_ignored_entries() {
        let dir = project();
        let listing = list_dir(dir.path(), Path::new("")).unwrap();
        assert_eq!(
            names(&listing),
            vec![
                (".git", true),
                ("src", false),
                ("target", true),
                (".gitignore", false),
                ("app.log", true),
                ("README.md", false),
            ]
        );
        assert_eq!(listing.entries[1].kind, EntryKind::Dir);
        assert_eq!(listing.entries[3].kind, EntryKind::File);
    }

    #[test]
    fn lists_a_subdirectory() {
        let dir = project();
        let listing = list_dir(dir.path(), Path::new("src")).unwrap();
        assert_eq!(names(&listing), vec![("nested", false), ("lib.rs", false)]);
    }

    #[test]
    fn refuses_paths_outside_the_root() {
        let dir = project();
        assert!(list_dir(dir.path(), Path::new("..")).is_err());
        assert!(read_file(dir.path(), Path::new("/etc/hosts")).is_err());
        assert!(read_file(dir.path(), Path::new("src/../../x")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinks_out_of_the_root() {
        let dir = project();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        assert!(read_file(dir.path(), Path::new("link/secret")).is_err());
    }

    #[test]
    fn reads_text_and_flags_binary_and_large_files() {
        let dir = project();
        assert_eq!(
            read_file(dir.path(), Path::new("src/lib.rs")).unwrap(),
            FileContent::Text("fn main() {}\n".into())
        );
        fs::write(dir.path().join("bin"), [0u8, 159, 146, 150, 0, 1]).unwrap();
        assert_eq!(
            read_file(dir.path(), Path::new("bin")).unwrap(),
            FileContent::Binary
        );
        let big = vec![b'a'; MAX_VIEW_FILE_BYTES as usize + 1];
        fs::write(dir.path().join("big.txt"), big).unwrap();
        assert!(matches!(
            read_file(dir.path(), Path::new("big.txt")).unwrap(),
            FileContent::TooLarge { .. }
        ));
    }

    #[test]
    fn writes_existing_files_inside_the_root_only() {
        let dir = project();
        write_file(dir.path(), Path::new("src/lib.rs"), "fn b() {}\n").unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("src/lib.rs")).unwrap(),
            "fn b() {}\n"
        );
        assert!(write_file(dir.path(), Path::new("src/new.rs"), "x").is_err());
        assert!(write_file(dir.path(), Path::new("../x"), "x").is_err());
        assert!(write_file(dir.path(), Path::new("src"), "x").is_err());
    }

    #[test]
    fn lists_files_kept_by_gitignore() {
        let dir = project();
        let (mut files, cut) = list_files(dir.path()).unwrap();
        files.sort();
        assert!(!cut);
        assert_eq!(
            files,
            vec![".gitignore", "README.md", "src/lib.rs", "src/nested/b.rs"]
        );
    }
}
