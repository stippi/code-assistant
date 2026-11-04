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

/// Where a file tool writes a file: inside the project, or outside it.
pub(crate) struct WriteTarget {
    pub explorer: Box<dyn CodeExplorer>,
    pub full_path: PathBuf,
    /// Outside the project: no format-on-save, no resource events.
    pub outside: bool,
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
        return Ok(WriteTarget {
            explorer,
            full_path: root.join(path),
            outside: false,
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
        outside: true,
    })
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
    }

    impl OutsideAccessMediator {
        pub(crate) fn new(decision: PermissionDecision) -> Arc<Self> {
            Arc::new(Self {
                decision,
                asked: Default::default(),
                accesses: Default::default(),
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
}
