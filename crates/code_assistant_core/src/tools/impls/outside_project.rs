//! Files outside the project a file tool works in: read or written only
//! after the user approves it.

use crate::tools::core::ToolContext;
use fs_explorer::CodeExplorer;
use std::path::{Path, PathBuf};
use tools_core::{FileAccess, PermissionRequestReason};

/// Session grant key: an "always" answer lets every file tool read (or
/// write) outside the project for the rest of the session. Reads and writes
/// are granted separately.
fn grant_key(access: FileAccess) -> String {
    format!("{}_outside_project", access.verb())
}

/// Ask the user, once for all of `paths`, to let `tool_name` read or write
/// them outside the project. `Err` carries the message to report for each
/// refused path.
pub(crate) async fn approve(
    context: &mut ToolContext<'_>,
    tool_name: &str,
    access: FileAccess,
    paths: &[PathBuf],
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut paths = paths.to_vec();
    paths.sort();
    paths.dedup();
    match context
        .request_escalation(
            &grant_key(access),
            tool_name,
            PermissionRequestReason::OutsideProject {
                access,
                paths: &paths,
            },
        )
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "The user denied the {} outside the project. Do not simply retry; \
             ask the user how to proceed.",
            access.verb()
        )),
        Err(e) => Err(format!("Access outside project root is not allowed ({e})")),
    }
}

/// Where a path a file tool was given leads.
#[derive(Debug)]
pub(crate) enum Resolved {
    /// Inside the project: the tool resolves it as usual.
    Inside,
    /// Outside the project, and the user allowed it: where it really points.
    Outside(PathBuf),
    /// Outside the project, and refused: the message to report for it.
    Refused(String),
}

/// Resolve each of `paths` (relative to `root`, or absolute) and ask the
/// user, once for all that leave the project, to let `tool_name` reach them.
/// The result is in the order of `paths`. Each path is resolved only here,
/// so a tool touches exactly what the user approved, even if a symlink
/// changes while the question is open.
pub(crate) async fn resolve_and_approve<'p>(
    context: &mut ToolContext<'_>,
    tool_name: &str,
    access: FileAccess,
    root: &Path,
    paths: impl IntoIterator<Item = &'p Path>,
) -> Vec<Resolved> {
    let outside: Vec<Option<PathBuf>> = paths
        .into_iter()
        .map(|path| fs_explorer::resolve_outside_root(root, path))
        .collect();
    let asked: Vec<PathBuf> = outside.iter().flatten().cloned().collect();
    let denial = approve(context, tool_name, access, &asked).await.err();
    outside
        .into_iter()
        .map(|outside| match (outside, &denial) {
            (None, _) => Resolved::Inside,
            (Some(_), Some(denial)) => Resolved::Refused(denial.clone()),
            (Some(path), None) => Resolved::Outside(path),
        })
        .collect()
}

/// Where a file tool writes a file: inside the project, or outside it.
pub(crate) struct WriteTarget {
    pub explorer: Box<dyn CodeExplorer>,
    pub full_path: PathBuf,
    /// The path relative to the project root, for format-on-save and
    /// resource events; `None` outside the project, which gets neither.
    pub project_path: Option<PathBuf>,
}

/// Where `tool_name` writes `path`: inside the project through `explorer`,
/// or — once the user approves — outside it through an explorer rooted at
/// the file's folder. `Err` carries the message for a refused path.
pub(crate) async fn write_target(
    context: &mut ToolContext<'_>,
    tool_name: &str,
    explorer: Box<dyn CodeExplorer>,
    path: &Path,
) -> Result<WriteTarget, String> {
    let root = explorer.root_dir();
    let Some(outside) = fs_explorer::resolve_outside_root(&root, path) else {
        let project_path = relative_to_root(&root, path);
        return Ok(WriteTarget {
            explorer,
            full_path: root.join(&project_path),
            project_path: Some(project_path),
        });
    };
    approve(
        context,
        tool_name,
        FileAccess::Write,
        std::slice::from_ref(&outside),
    )
    .await?;
    Ok(WriteTarget {
        explorer: explorer_for(&outside),
        full_path: outside,
        project_path: None,
    })
}

/// `path`, known to lie inside `root`, relative to it: an absolute path
/// loses the root (also when it reaches the root through a symlink, such as
/// macOS's `/tmp`). Falls back to `path` itself when that fails.
pub(crate) fn relative_to_root(root: &Path, path: &Path) -> PathBuf {
    if !path.is_absolute() {
        return path.to_path_buf();
    }
    let canonical = || {
        path.canonicalize().ok().or_else(|| {
            let parent = path.parent()?.canonicalize().ok()?;
            Some(parent.join(path.file_name()?))
        })
    };
    path.strip_prefix(root)
        .ok()
        .map(Path::to_path_buf)
        .or_else(|| {
            canonical()
                .and_then(|canonical| canonical.strip_prefix(root).ok().map(Path::to_path_buf))
        })
        .unwrap_or_else(|| path.to_path_buf())
}

/// An explorer for a path outside the project: rooted at the path itself
/// when it is a folder, else at its parent.
pub(crate) fn explorer_for(path: &Path) -> Box<dyn CodeExplorer> {
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    Box::new(fs_explorer::Explorer::new(dir.to_path_buf()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mocks::ToolTestFixture;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tools_core::{PermissionDecision, PermissionMediator, PermissionRequest};

    /// Answers every request with one decision and records the paths asked
    /// about.
    pub(crate) struct OutsideAccessMediator {
        decision: PermissionDecision,
        pub(crate) asked: Mutex<Vec<Vec<PathBuf>>>,
        pub(crate) accesses: Mutex<Vec<FileAccess>>,
        /// Runs while the question is open, before the answer.
        while_asking: Option<Box<dyn Fn() + Send + Sync>>,
    }

    impl OutsideAccessMediator {
        pub(crate) fn new(decision: PermissionDecision) -> Arc<Self> {
            Arc::new(Self {
                decision,
                asked: Default::default(),
                accesses: Default::default(),
                while_asking: None,
            })
        }

        /// Like [`Self::new`], running `while_asking` before each answer.
        pub(crate) fn while_asking(
            decision: PermissionDecision,
            while_asking: impl Fn() + Send + Sync + 'static,
        ) -> Arc<Self> {
            Arc::new(Self {
                decision,
                asked: Default::default(),
                accesses: Default::default(),
                while_asking: Some(Box::new(while_asking)),
            })
        }
    }

    #[async_trait::async_trait]
    impl PermissionMediator for OutsideAccessMediator {
        async fn request_permission(
            &self,
            request: PermissionRequest<'_>,
        ) -> anyhow::Result<PermissionDecision> {
            let PermissionRequestReason::OutsideProject { access, paths } = request.reason else {
                panic!("unexpected permission request: {:?}", request.reason);
            };
            self.asked.lock().unwrap().push(paths.to_vec());
            self.accesses.lock().unwrap().push(access);
            if let Some(while_asking) = &self.while_asking {
                while_asking();
            }
            Ok(self.decision)
        }
    }

    /// The smallest PNG: one pixel.
    const PIXEL_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2,
        0, 0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0, 0,
        3, 1, 1, 0, 201, 254, 146, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    /// A project folder `proj` with `inside.txt`, next to `outside.txt` and a
    /// folder `other` holding `note.txt` and `pixel.png`.
    pub(crate) fn outside_setup() -> (tempfile::TempDir, ToolTestFixture) {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("inside.txt"), "inside").unwrap();
        std::fs::write(dir.path().join("outside.txt"), "one\ntwo\nthree").unwrap();
        let other = dir.path().join("other");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("note.txt"), "a needle here").unwrap();
        std::fs::write(other.join("pixel.png"), PIXEL_PNG).unwrap();
        let manager = crate::mocks::MockProjectManager::default().with_project_path(
            "proj",
            project.clone(),
            Box::new(fs_explorer::Explorer::new(project)),
        );
        (dir, ToolTestFixture::with_project_manager(manager))
    }

    /// One call per file tool, each reaching into `other`.
    fn outside_calls() -> Vec<(&'static str, serde_json::Value, &'static str)> {
        vec![
            (
                "read_files",
                json!({"paths": ["../other/note.txt"]}),
                "needle",
            ),
            ("list_files", json!({"paths": ["../other"]}), "note.txt"),
            (
                "search_files",
                json!({"regex": "needle", "paths": ["../other"]}),
                "note.txt",
            ),
            (
                "glob_files",
                json!({"pattern": "../other/*.txt"}),
                "note.txt",
            ),
            (
                "view_images",
                json!({"paths": ["../other/pixel.png"]}),
                "pixel.png",
            ),
        ]
    }

    async fn invoke(
        fixture: &mut ToolTestFixture,
        tool: &str,
        mut params: serde_json::Value,
    ) -> anyhow::Result<String> {
        params["project"] = json!("proj");
        let registry = crate::tools::test_registry();
        let result = registry
            .get(tool)
            .unwrap()
            .invoke(&mut fixture.context(), &mut params)
            .await?;
        let output = result
            .as_render()
            .render(&mut tools_core::ResourcesTracker::new());
        anyhow::ensure!(result.is_success(), "{output}");
        Ok(output)
    }

    #[tokio::test]
    async fn every_file_tool_reads_outside_the_project_once_allowed() {
        for (tool, params, expected) in outside_calls() {
            let (dir, fixture) = outside_setup();
            let mediator = OutsideAccessMediator::new(PermissionDecision::GrantedOnce);
            let mut fixture = fixture.with_permission_handler(mediator.clone());

            let output = invoke(&mut fixture, tool, params).await.unwrap();

            assert!(output.contains(expected), "{tool}: {output}");
            let asked = mediator.asked.lock().unwrap();
            assert_eq!(asked.len(), 1, "{tool} should ask once");
            let other = dir.path().join("other").canonicalize().unwrap();
            assert!(asked[0][0].starts_with(&other), "{tool}: {asked:?}");
        }
    }

    #[tokio::test]
    async fn every_file_tool_refuses_outside_the_project_when_denied() {
        for (tool, params, _) in outside_calls() {
            let (_dir, fixture) = outside_setup();
            let mediator = OutsideAccessMediator::new(PermissionDecision::Denied);
            let mut fixture = fixture.with_permission_handler(mediator);

            let error = invoke(&mut fixture, tool, params).await.unwrap_err();

            let error = error.to_string();
            assert!(error.contains("denied"), "{tool}: {error}");
            assert!(!error.contains("needle"), "{tool}: {error}");
        }
    }

    #[tokio::test]
    async fn file_tools_refuse_outside_the_project_when_nobody_can_be_asked() {
        for (tool, params, _) in outside_calls() {
            let (_dir, mut fixture) = outside_setup();

            let error = invoke(&mut fixture, tool, params).await.unwrap_err();

            assert!(
                error.to_string().contains("outside project root"),
                "{tool}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn a_session_grant_covers_every_file_tool() {
        let (_dir, fixture) = outside_setup();
        let mediator = OutsideAccessMediator::new(PermissionDecision::GrantedSession);
        let mut fixture = fixture.with_permission_handler(mediator.clone());

        for (tool, params, expected) in outside_calls() {
            let output = invoke(&mut fixture, tool, params).await.unwrap();
            assert!(output.contains(expected), "{tool}: {output}");
        }
        assert_eq!(mediator.asked.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn absolute_paths_inside_the_project_need_no_approval() {
        let (dir, fixture) = outside_setup();
        let mediator = OutsideAccessMediator::new(PermissionDecision::Denied);
        let mut fixture = fixture.with_permission_handler(mediator.clone());
        let inside = dir.path().join("proj/inside.txt");

        let output = invoke(
            &mut fixture,
            "read_files",
            json!({"paths": [inside.to_str().unwrap()]}),
        )
        .await
        .unwrap();

        assert!(output.contains("inside"));
        assert!(mediator.asked.lock().unwrap().is_empty());
    }

    /// One call per writing tool, each changing something in `other`, with
    /// a check of the result on disk.
    #[allow(clippy::type_complexity)]
    fn outside_writes() -> Vec<(&'static str, serde_json::Value, fn(&Path) -> bool)> {
        vec![
            (
                "write_file",
                json!({"path": "../other/new.txt", "content": "written"}),
                |other| {
                    std::fs::read_to_string(other.join("new.txt"))
                        .is_ok_and(|content| content.trim_end() == "written")
                },
            ),
            (
                "edit",
                json!({"path": "../other/note.txt", "old_text": "needle", "new_text": "pin"}),
                |other| {
                    std::fs::read_to_string(other.join("note.txt"))
                        .unwrap()
                        .trim_end()
                        == "a pin here"
                },
            ),
            (
                "replace_in_file",
                json!({
                    "path": "../other/note.txt",
                    "diff": "<<<<<<< SEARCH\nneedle\n=======\npin\n>>>>>>> REPLACE"
                }),
                |other| {
                    std::fs::read_to_string(other.join("note.txt"))
                        .unwrap()
                        .trim_end()
                        == "a pin here"
                },
            ),
            (
                "delete_files",
                json!({"paths": ["../other/note.txt"]}),
                |other| !other.join("note.txt").exists(),
            ),
        ]
    }

    #[tokio::test]
    async fn every_writing_tool_writes_outside_the_project_once_allowed() {
        for (tool, params, written) in outside_writes() {
            let (dir, fixture) = outside_setup();
            let mediator = OutsideAccessMediator::new(PermissionDecision::GrantedOnce);
            let mut fixture = fixture.with_permission_handler(mediator.clone());

            invoke(&mut fixture, tool, params).await.unwrap();

            assert!(written(&dir.path().join("other")), "{tool} did not write");
            assert_eq!(
                *mediator.accesses.lock().unwrap(),
                vec![FileAccess::Write],
                "{tool}"
            );
        }
    }

    #[tokio::test]
    async fn every_writing_tool_leaves_outside_files_alone_when_denied() {
        for (tool, params, written) in outside_writes() {
            let (dir, fixture) = outside_setup();
            let mediator = OutsideAccessMediator::new(PermissionDecision::Denied);
            let mut fixture = fixture.with_permission_handler(mediator);

            let error = invoke(&mut fixture, tool, params).await.unwrap_err();

            assert!(error.to_string().contains("denied"), "{tool}: {error}");
            assert!(!written(&dir.path().join("other")), "{tool} wrote anyway");
            assert_eq!(
                std::fs::read_to_string(dir.path().join("other/note.txt")).unwrap(),
                "a needle here"
            );
        }
    }

    #[tokio::test]
    async fn writing_tools_refuse_outside_the_project_when_nobody_can_be_asked() {
        for (tool, params, written) in outside_writes() {
            let (dir, mut fixture) = outside_setup();

            let error = invoke(&mut fixture, tool, params).await.unwrap_err();

            assert!(
                error.to_string().contains("outside project root"),
                "{tool}: {error}"
            );
            assert!(!written(&dir.path().join("other")), "{tool} wrote anyway");
        }
    }

    #[tokio::test]
    async fn a_session_grant_for_reads_does_not_allow_writes() {
        let (dir, fixture) = outside_setup();
        let mediator = OutsideAccessMediator::new(PermissionDecision::GrantedSession);
        let mut fixture = fixture.with_permission_handler(mediator.clone());

        let read = json!({"paths": ["../other/note.txt"]});
        invoke(&mut fixture, "read_files", read.clone())
            .await
            .unwrap();
        let write = json!({"path": "../other/new.txt", "content": "written"});
        invoke(&mut fixture, "write_file", write.clone())
            .await
            .unwrap();
        // Both grants are remembered now: neither asks again.
        invoke(&mut fixture, "read_files", read).await.unwrap();
        invoke(&mut fixture, "write_file", write).await.unwrap();

        assert_eq!(
            *mediator.accesses.lock().unwrap(),
            vec![FileAccess::Read, FileAccess::Write]
        );
        assert!(dir.path().join("other/new.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_tool_uses_the_path_the_user_approved_even_if_a_symlink_changes_meanwhile() {
        let (dir, fixture) = outside_setup();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        for folder in [&first, &second] {
            std::fs::create_dir(folder).unwrap();
            std::fs::write(folder.join("x.txt"), "x").unwrap();
        }
        let link = dir.path().join("proj/link");
        std::os::unix::fs::symlink(&first, &link).unwrap();
        let mediator = {
            let (link, second) = (link.clone(), second.clone());
            OutsideAccessMediator::while_asking(PermissionDecision::GrantedOnce, move || {
                std::fs::remove_file(&link).unwrap();
                std::os::unix::fs::symlink(&second, &link).unwrap();
            })
        };
        let mut fixture = fixture.with_permission_handler(mediator.clone());

        invoke(
            &mut fixture,
            "delete_files",
            json!({"paths": ["link/x.txt"]}),
        )
        .await
        .unwrap();

        assert!(
            !first.join("x.txt").exists(),
            "the approved file is deleted"
        );
        assert!(second.join("x.txt").exists(), "the other file is not");
    }

    #[tokio::test]
    async fn search_files_says_nothing_about_an_outside_path_before_it_is_approved() {
        let (_dir, fixture) = outside_setup();
        let mediator = OutsideAccessMediator::new(PermissionDecision::Denied);
        let mut fixture = fixture.with_permission_handler(mediator.clone());

        let error = invoke(
            &mut fixture,
            "search_files",
            json!({"regex": "x", "paths": ["../missing"]}),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("denied"), "{error}");
        assert!(!error.contains("not found"), "{error}");
        assert_eq!(mediator.asked.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_absolute_glob_inside_the_project_matches_without_asking() {
        let (dir, fixture) = outside_setup();
        let mediator = OutsideAccessMediator::new(PermissionDecision::Denied);
        let mut fixture = fixture.with_permission_handler(mediator.clone());
        let pattern = dir.path().join("proj").join("*.txt");

        let output = invoke(
            &mut fixture,
            "glob_files",
            json!({"pattern": pattern.to_str().unwrap()}),
        )
        .await
        .unwrap();

        assert!(output.contains("inside.txt"), "{output}");
        assert!(mediator.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn absolute_paths_inside_the_root_become_relative() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        // The explorer's root is canonical; on macOS the temp dir is not.
        let root = dir.path().canonicalize().unwrap();

        for path in [
            dir.path().join("src/lib.rs"),
            root.join("src/lib.rs"),
            dir.path().join("src/new.rs"),
        ] {
            let relative = relative_to_root(&root, &path);
            assert!(relative.is_relative(), "{}", path.display());
            assert!(relative.starts_with("src"), "{}", relative.display());
        }
        assert_eq!(
            relative_to_root(&root, Path::new("src/lib.rs")),
            Path::new("src/lib.rs")
        );
    }
}
