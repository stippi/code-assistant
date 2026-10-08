//! Tool results stored as blobs: a frontend gets their outputs on demand,
//! an agent gets all of them.

use super::tests::{test_service_with_llm, test_service_with_manager};
use super::*;
use crate::mocks::{MockLLMProvider, create_test_response_text};
use crate::persistence::FileSessionPersistence;
use crate::session::SessionSnapshot;
use crate::session::{TurnDispatch, TurnRequest};
use crate::ui::ToolStatus;
use agent_core::{SerializedToolExecution, ToolRequest};
use llm::{ContentBlock, Message};
use std::path::Path;

/// The session store under `root`, as another process sees it.
fn store(root: &Path) -> FileSessionPersistence {
    FileSessionPersistence::new_with_root_dir(root.to_path_buf())
}

/// A recorded `execute_command` run.
fn command(id: &str, output: &str, success: bool) -> SerializedToolExecution {
    SerializedToolExecution {
        tool_request: ToolRequest {
            id: id.into(),
            name: "execute_command".into(),
            input: serde_json::json!({ "command_line": "make" }),
            start_offset: None,
            end_offset: None,
        },
        tool_name: "execute_command".into(),
        result_json: serde_json::json!({
            "project": "p",
            "command_line": "make",
            "working_dir": null,
            "output": output,
            "success": success,
        }),
    }
}

/// Store a conversation in which the assistant ran `executions`, each
/// answered by a tool result (an error where the run failed).
fn store_runs(root: &Path, session_id: &str, executions: Vec<(SerializedToolExecution, bool)>) {
    let uses = executions
        .iter()
        .map(|(exec, _)| {
            ContentBlock::new_tool_use(
                exec.tool_request.id.clone(),
                exec.tool_name.clone(),
                exec.tool_request.input.clone(),
            )
        })
        .collect();
    let results = executions
        .iter()
        .map(|(exec, success)| match success {
            true => ContentBlock::new_tool_result(exec.tool_request.id.clone(), ""),
            false => ContentBlock::new_error_tool_result(exec.tool_request.id.clone(), ""),
        })
        .collect();
    store(root)
        .update_entry(session_id, |session| {
            session.add_message(Message::new_user("build it"));
            session.add_message(Message::new_assistant_content(uses));
            session.add_message(Message::new_user_content(results));
            session.tool_executions = executions.into_iter().map(|(exec, _)| exec).collect();
            Ok(())
        })
        .unwrap();
}

fn result<'a>(snapshot: &'a SessionSnapshot, tool_id: &str) -> &'a ToolResultData {
    snapshot
        .tool_results
        .iter()
        .find(|result| result.tool_id == tool_id)
        .unwrap()
}

#[tokio::test]
async fn showing_a_session_leaves_large_successful_outputs_unread() {
    let tmp = tempfile::tempdir().unwrap();
    let (service, _) = test_service_with_manager(tmp.path());
    let id = service.create_session(None, None).await.unwrap();
    let large = "x".repeat(10_000);
    store_runs(
        tmp.path(),
        &id,
        vec![
            (command("large", &large, true), true),
            (command("failed", &"e".repeat(10_000), false), false),
            (command("small", "ok", true), true),
        ],
    );
    // Without its blob, reading the large result would fail.
    let stored = store(tmp.path())
        .load_chat_session_unresolved(&id)
        .unwrap()
        .unwrap();
    let hash = stored.tool_executions[0].result_json["$blob"]
        .as_str()
        .unwrap();
    let blob = store(tmp.path())
        .layout()
        .blobs_dir(&id)
        .unwrap()
        .join(format!("{hash}.json"));
    let blob_content = std::fs::read(&blob).unwrap();
    std::fs::remove_file(&blob).unwrap();

    let snapshot = service.load_session(id.clone(), None).await.unwrap();

    let deferred = result(&snapshot, "large");
    assert!(deferred.output_deferred);
    assert_eq!(deferred.status, ToolStatus::Success);
    assert_eq!(deferred.output, None);
    // Failures and small results come complete.
    let failed = result(&snapshot, "failed");
    assert!(!failed.output_deferred);
    assert_eq!(failed.status, ToolStatus::Error);
    assert!(failed.output.as_ref().unwrap().contains("eeee"));
    let small = result(&snapshot, "small");
    assert!(!small.output_deferred);
    assert!(small.output.as_ref().unwrap().contains("ok"));

    std::fs::write(&blob, blob_content).unwrap();
    let loaded = service
        .load_tool_outputs(id.clone(), vec!["large".into(), "unknown".into()])
        .await
        .unwrap();
    // One call for all of them; a tool the session doesn't know is left out.
    assert_eq!(loaded.len(), 1);
    let loaded = &loaded[0];
    assert_eq!(loaded.tool_id, "large");
    assert!(!loaded.output_deferred);
    assert_eq!(loaded.status, ToolStatus::Success);
    assert!(loaded.output.as_ref().unwrap().contains(&large));
}

#[cfg(unix)]
#[tokio::test]
async fn a_session_shown_again_is_not_read_again_while_unchanged() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let (service, _) = test_service_with_manager(tmp.path());
    let id = service.create_session(None, None).await.unwrap();
    service.load_session(id.clone(), None).await.unwrap();

    // Unchanged on disk: the resident session is shown as it is, even when
    // its journal has become unreadable meanwhile.
    let journal = store(tmp.path()).layout().journal(&id).unwrap();
    let stored = std::fs::read(&journal).unwrap();
    std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o000)).unwrap();
    let shown = service.load_session(id.clone(), None).await;
    std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o600)).unwrap();
    shown.unwrap();

    // Changed on disk: shown as changed.
    store_runs(tmp.path(), &id, vec![(command("new", "ok", true), true)]);
    assert_ne!(std::fs::read(&journal).unwrap(), stored);
    let snapshot = service.load_session(id.clone(), None).await.unwrap();
    assert_eq!(result(&snapshot, "new").status, ToolStatus::Success);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_sees_the_large_outputs_of_earlier_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let llm = MockLLMProvider::new(vec![Ok(create_test_response_text("done"))]);
    let (service, _) = test_service_with_llm(tmp.path(), llm.clone().into_factory());
    let id = service.create_session(None, None).await.unwrap();
    let large = "x".repeat(10_000);
    store_runs(
        tmp.path(),
        &id,
        vec![(command("large", &large, true), true)],
    );
    service.load_session(id.clone(), None).await.unwrap();

    let TurnDispatch::Started(handle) = service
        .start_turn_if_idle(id.clone(), TurnRequest::text("and now?"))
        .await
        .unwrap()
    else {
        panic!("fresh session busy")
    };
    handle.wait().await.unwrap();

    let requests = llm.get_requests();
    let rendered = serde_json::to_string(&requests[0].messages).unwrap();
    assert!(rendered.contains(&large), "the stored output was not sent");
}
