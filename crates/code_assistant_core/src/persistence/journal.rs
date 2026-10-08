//! The session record as an append-only journal (`journal.jsonl`).
//!
//! Each line is one [`Record`]. Loading folds them in order: the last
//! `header` wins, `node` and `exec` records replace earlier ones with the
//! same id. Changing a session appends the records that differ instead of
//! rewriting the whole session; [`needs_compaction`] decides when the
//! journal is rewritten as one record per live entry.
//!
//! Every append ends with a newline. A file that doesn't end in one was cut
//! off mid-write, and its last partial line is dropped; any other line that
//! doesn't parse is an error (a newer version may have written it).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use super::{ChatSession, MessageNode, SerializedToolExecution};
use crate::utils::file_utils::atomic_write;

/// One line of the journal.
#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Record {
    /// Everything about the session except its nodes and executions.
    Header {
        session: Box<ChatSession>,
    },
    Node {
        node: MessageNode,
    },
    Exec {
        exec: SerializedToolExecution,
    },
}

/// A session folded from its journal, and how many records that took.
pub struct Folded {
    pub session: ChatSession,
    pub records: usize,
}

/// The header record of a session: the session without its nodes and
/// executions (which have records of their own) and without legacy linear
/// messages (which loading has moved into the tree).
pub fn header(session: &ChatSession) -> Record {
    Record::Header {
        session: Box::new(session.without_conversation()),
    }
}

/// The records describing a whole session, for a new or compacted journal.
pub fn snapshot(session: &ChatSession) -> Vec<Record> {
    std::iter::once(header(session))
        .chain(
            session
                .message_nodes
                .values()
                .map(|node| Record::Node { node: node.clone() }),
        )
        .chain(
            session
                .tool_executions
                .iter()
                .map(|exec| Record::Exec { exec: exec.clone() }),
        )
        .collect()
}

/// Read and fold a journal. `None` when the session has none.
pub fn read(path: &Path) -> Result<Option<Folded>> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let complete = match content.rfind('\n') {
        Some(end) => &content[..end],
        None => "",
    };

    let mut session: Option<ChatSession> = None;
    let mut executions: Vec<SerializedToolExecution> = Vec::new();
    let mut execution_index: HashMap<String, usize> = HashMap::new();
    let mut nodes = std::collections::BTreeMap::new();
    let mut records = 0;
    for (number, line) in complete.lines().enumerate() {
        let record: Record = serde_json::from_str(line)
            .with_context(|| format!("{}: line {}", path.display(), number + 1))?;
        records += 1;
        match record {
            Record::Header { session: header } => session = Some(*header),
            Record::Node { node } => {
                nodes.insert(node.id, node);
            }
            Record::Exec { exec } => match execution_index.get(&exec.tool_request.id) {
                Some(&index) => executions[index] = exec,
                None => {
                    execution_index.insert(exec.tool_request.id.clone(), executions.len());
                    executions.push(exec);
                }
            },
        }
    }

    let mut session = session.with_context(|| format!("{}: no header record", path.display()))?;
    session.message_nodes = nodes;
    session.tool_executions = executions;
    Ok(Some(Folded { session, records }))
}

/// Append records in one write. The caller holds the session's entry lock.
pub fn append(path: &Path, records: &[Record]) -> Result<()> {
    if records.is_empty() {
        return Ok(());
    }
    let bytes = encode(records)?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(&bytes)?;
    file.sync_data()?;
    Ok(())
}

/// Replace the journal with `records` (temp file + rename).
pub fn write(path: &Path, records: &[Record]) -> Result<()> {
    atomic_write(path, &encode(records)?)
}

fn encode(records: &[Record]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

/// Whether a journal of `records` lines is worth rewriting for a session of
/// this size: when it holds more than twice the records a snapshot would.
pub fn needs_compaction(records: usize, session: &ChatSession) -> bool {
    let live = 1 + session.message_nodes.len() + session.tool_executions.len();
    records > 2 * live + 16
}

/// The records that turn `before` into `after`: a header if anything outside
/// the conversation changed, and every node and execution that is new or
/// differs. Nodes and executions are never removed.
pub fn diff(before: &ChatSession, after: &ChatSession) -> Result<Vec<Record>> {
    let mut records = Vec::new();
    let header_after = header(after);
    if serde_json::to_vec(&header(before))? != serde_json::to_vec(&header_after)? {
        records.push(header_after);
    }
    for (id, node) in &after.message_nodes {
        let unchanged = match before.message_nodes.get(id) {
            Some(old) => serde_json::to_vec(old)? == serde_json::to_vec(node)?,
            None => false,
        };
        if !unchanged {
            records.push(Record::Node { node: node.clone() });
        }
    }
    let before_executions: HashMap<&str, &SerializedToolExecution> = before
        .tool_executions
        .iter()
        .map(|exec| (exec.tool_request.id.as_str(), exec))
        .collect();
    for exec in &after.tool_executions {
        let unchanged = before_executions
            .get(exec.tool_request.id.as_str())
            .is_some_and(|old| {
                old.tool_name == exec.tool_name
                    && old.result_json == exec.result_json
                    && old.tool_request.input == exec.tool_request.input
            });
        if !unchanged {
            records.push(Record::Exec { exec: exec.clone() });
        }
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionConfig;
    use llm::Message;
    use tempfile::tempdir;

    fn session() -> ChatSession {
        ChatSession::new_empty("s".into(), "s".into(), SessionConfig::default(), None)
    }

    fn exec(id: &str, output: &str) -> SerializedToolExecution {
        SerializedToolExecution {
            tool_request: agent_core::ToolRequest {
                id: id.into(),
                name: "read_files".into(),
                input: serde_json::json!({}),
                start_offset: None,
                end_offset: None,
            },
            result_json: serde_json::json!({ "output": output }),
            tool_name: "read_files".into(),
        }
    }

    #[test]
    fn later_records_replace_earlier_ones_in_place() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let mut session = session();
        session.add_message(Message::new_user("first"));
        session.tool_executions = vec![exec("a", "1"), exec("b", "2")];
        write(&path, &snapshot(&session)).unwrap();

        let mut renamed = session.clone();
        renamed.name = "renamed".into();
        append(
            &path,
            &[
                Record::Exec {
                    exec: exec("a", "updated"),
                },
                header(&renamed),
            ],
        )
        .unwrap();

        let folded = read(&path).unwrap().unwrap();
        assert_eq!(folded.records, 6);
        assert_eq!(folded.session.name, "renamed");
        assert_eq!(folded.session.message_count(), 1);
        let outputs: Vec<_> = folded
            .session
            .tool_executions
            .iter()
            .map(|e| (e.tool_request.id.as_str(), e.result_json["output"].clone()))
            .collect();
        assert_eq!(
            outputs,
            [
                ("a", serde_json::json!("updated")),
                ("b", serde_json::json!("2"))
            ]
        );
    }

    #[test]
    fn a_line_cut_off_mid_write_is_dropped() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        write(&path, &snapshot(&session())).unwrap();
        let mut content = std::fs::read(&path).unwrap();
        content.extend_from_slice(br#"{"t":"node","node":{"id":1,"#);
        std::fs::write(&path, content).unwrap();

        let folded = read(&path).unwrap().unwrap();
        assert_eq!(folded.records, 1);
    }

    #[test]
    fn a_complete_line_that_does_not_parse_is_an_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        write(&path, &snapshot(&session())).unwrap();
        let mut content = std::fs::read(&path).unwrap();
        content.extend_from_slice(b"{\"t\":\"from_the_future\"}\n");
        std::fs::write(&path, content).unwrap();

        assert!(read(&path).is_err());
    }

    #[test]
    fn diff_holds_only_what_changed() {
        let mut before = session();
        before.add_message(Message::new_user("first"));
        before.tool_executions = vec![exec("a", "1")];

        assert!(diff(&before, &before.clone()).unwrap().is_empty());

        let mut after = before.clone();
        after.add_message(Message::new_user("second"));
        after.tool_executions.push(exec("b", "2"));
        let kinds: Vec<_> = diff(&before, &after)
            .unwrap()
            .iter()
            .map(|record| match record {
                Record::Header { .. } => "header".to_string(),
                Record::Node { node } => format!("node {}", node.id),
                Record::Exec { exec } => format!("exec {}", exec.tool_request.id),
            })
            .collect();
        // Adding a message moves the active path, which lives in the header.
        assert_eq!(kinds, ["header", "node 2", "exec b"]);
    }
}
