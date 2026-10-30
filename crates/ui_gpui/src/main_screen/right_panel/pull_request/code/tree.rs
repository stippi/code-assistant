//! The Code tab's file tree as rows, after T3 Code's `DiffFileTree`
//! (`@pierre/trees` with `flattenEmptyDirectories`): folders in the order
//! the list shows their files, a folder holding just one folder drawn as
//! one row (`src/main`), collapsed folders hiding what is below.

use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeRow {
    Folder {
        /// The folder's path, which keys its collapsed state.
        path: String,
        /// Its name, or the names of the folders drawn as one row.
        name: String,
        depth: usize,
        collapsed: bool,
    },
    File {
        /// The shown file's index.
        file: usize,
        name: String,
        depth: usize,
    },
}

#[derive(Default)]
struct Node {
    name: String,
    folders: Vec<Node>,
    files: Vec<(usize, String)>,
    /// Folders and files in the order they first appear.
    order: Vec<Entry>,
}

#[derive(Clone, Copy)]
enum Entry {
    Folder(usize),
    File(usize),
}

impl Node {
    fn insert(&mut self, parts: &[&str], file: usize) {
        match parts {
            [] => {}
            [name] => {
                self.order.push(Entry::File(self.files.len()));
                self.files.push((file, (*name).to_owned()));
            }
            [folder, rest @ ..] => {
                let ix = match self.folders.iter().position(|f| f.name == *folder) {
                    Some(ix) => ix,
                    None => {
                        self.order.push(Entry::Folder(self.folders.len()));
                        self.folders.push(Node {
                            name: (*folder).to_owned(),
                            ..Node::default()
                        });
                        self.folders.len() - 1
                    }
                };
                self.folders[ix].insert(rest, file);
            }
        }
    }

    fn flatten(
        &self,
        parent: &str,
        depth: usize,
        collapsed: &HashSet<String>,
        out: &mut Vec<TreeRow>,
    ) {
        for entry in &self.order {
            match *entry {
                Entry::File(ix) => {
                    let (file, name) = &self.files[ix];
                    out.push(TreeRow::File {
                        file: *file,
                        name: name.clone(),
                        depth,
                    });
                }
                Entry::Folder(ix) => {
                    let mut folder = &self.folders[ix];
                    let mut name = folder.name.clone();
                    while folder.files.is_empty() && folder.folders.len() == 1 {
                        folder = &folder.folders[0];
                        name = format!("{name}/{}", folder.name);
                    }
                    let path = match parent {
                        "" => name.clone(),
                        parent => format!("{parent}/{name}"),
                    };
                    let is_collapsed = collapsed.contains(&path);
                    out.push(TreeRow::Folder {
                        path: path.clone(),
                        name,
                        depth,
                        collapsed: is_collapsed,
                    });
                    if !is_collapsed {
                        folder.flatten(&path, depth + 1, collapsed, out);
                    }
                }
            }
        }
    }
}

/// The rows for the shown files' `paths` (index = shown file).
pub fn tree_rows(paths: &[&str], collapsed: &HashSet<String>) -> Vec<TreeRow> {
    let mut root = Node::default();
    for (file, path) in paths.iter().enumerate() {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        root.insert(&parts, file);
    }
    let mut rows = Vec::new();
    root.flatten("", 0, collapsed, &mut rows);
    rows
}

/// Every folder path of the rows (with all expanded), for collapsing all.
pub fn folder_paths(paths: &[&str]) -> HashSet<String> {
    tree_rows(paths, &HashSet::new())
        .into_iter()
        .filter_map(|row| match row {
            TreeRow::Folder { path, .. } => Some(path),
            TreeRow::File { .. } => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                TreeRow::Folder {
                    name,
                    depth,
                    collapsed,
                    ..
                } => format!(
                    "{}{name}/{}",
                    "  ".repeat(*depth),
                    if *collapsed { " +" } else { "" }
                ),
                TreeRow::File { file, name, depth } => {
                    format!("{}{name} #{file}", "  ".repeat(*depth))
                }
            })
            .collect()
    }

    const PATHS: [&str; 4] = [
        "crates/github/src/ops/workflows.rs",
        "crates/github/src/ops/mod.rs",
        "crates/github/tests/operations.rs",
        "README.md",
    ];

    #[test]
    fn folders_holding_one_folder_are_one_row() {
        assert_eq!(
            shape(&tree_rows(&PATHS, &HashSet::new())),
            [
                "crates/github/",
                "  src/ops/",
                "    workflows.rs #0",
                "    mod.rs #1",
                "  tests/",
                "    operations.rs #2",
                "README.md #3",
            ]
        );
    }

    #[test]
    fn a_collapsed_folder_hides_what_is_below() {
        let collapsed = HashSet::from(["crates/github/src/ops".to_owned()]);
        assert_eq!(
            shape(&tree_rows(&PATHS, &collapsed)),
            [
                "crates/github/",
                "  src/ops/ +",
                "  tests/",
                "    operations.rs #2",
                "README.md #3",
            ]
        );
        assert_eq!(
            folder_paths(&PATHS),
            HashSet::from([
                "crates/github".to_owned(),
                "crates/github/src/ops".to_owned(),
                "crates/github/tests".to_owned(),
            ])
        );
    }
}
