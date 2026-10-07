# Session storage: folders, blobs and an append-only journal

Status: design, not implemented.

Every session is one JSON file today (`sessions/chat_<id>.json`), and every
agent checkpoint rewrites it completely. With browser screenshots and large
tool outputs this gets expensive fast. This document describes a storage
layout that keeps checkpoints proportional to what changed.

## The problem

### Where the bytes are

Measured on `chat_6a677747_17a_0.json` (238 MB):

| Part                                   | Size        |
|----------------------------------------|-------------|
| `message_nodes` (124 nodes)            | 110 KB      |
| `tool_executions` (61 records)         | 232 MB      |
| the 10 executions larger than 4 KB     | 99.98 %     |
| one `delete_files` result              | 237 MB      |

The conversation itself is small. A node's `tool_result` blocks are empty
and refer to their execution by `tool_use_id`; the output lives in
`tool_executions` as `result_json`. The big records are screenshots (base64)
and outputs that keep file contents for the UI, for example
`DeleteFilesOutput::deleted_contents`, which holds every deleted file for the
diff view and is never shown to the LLM.

The sessions directory holds 1148 session files, 2.3 GB in total; five of
them are between 150 and 400 MB.

### What a checkpoint costs

`AgentRuntime::checkpoint` already produces a delta: `SessionCheckpoint`
carries only `changed_nodes` and `changed_executions` plus the small
always-current fields (`active_path`, `plan`, counters). The persistence
layer turns it back into a full rewrite. `FileSessionPersistence::update_entry`

1. reads and parses the whole session file,
2. merges the delta (`ChatSession::apply_checkpoint`),
3. clones the session and writes it completely with
   `atomic_write_json` (pretty-printed, temp file + rename),
4. reads and rewrites `metadata.json`.

The agent loop checkpoints several times per step (`append_message_with_node_id`
alone triggers one), so a step writes the session two or three times. The
total cost grows quadratically with session length. One GPUI run of about an
hour with a browser session that grew to 18 MB made macOS file a
disk-writes diagnostic report: 8.6 GB written, mostly from
`SessionManager::commit_checkpoint`. All of this happens while the global
`SessionManager` mutex is held.

## Layout

One folder per session, named after the session ID (see below):

```
sessions/
  metadata.json                      global index, as today
  lifecycle.json                     visits and settlement, as today
  -Users-me-workspace-code-assistant/
    2026-10-07-001/
      journal.jsonl                  append-only session records
      blobs/<sha256>.json            externalized tool results, immutable
      ui_state.json
      draft.json                     moves here from <base>/drafts/
      diag.log
      agent.lock
      entry.lock
    2026-10-07-002/
      ...
```

Consequences:

- `delete_chat_session` becomes `remove_dir_all` plus the index and
  lifecycle entries.
- `SessionWatcher` currently watches `sessions/` non-recursively and derives
  the session from the file name. It has to watch recursively and react to
  `journal.jsonl` and `agent.lock` only, mapping the path back to the ID
  (`<project>/<date>-<n>`).
- `FileDraftStore` and the `ui_state` store resolve their paths through the
  session folder instead of a flat directory.

## Session IDs

IDs are readable and say where a session lives, without reading
`metadata.json`:

```
<project-slug>/<YYYY-MM-DD>-<NNN>
-Users-me-workspace-code-assistant/2026-10-07-003
```

- **Project slug**: the session's project root (`SessionConfig::init_path`)
  with path separators replaced by `-`, the scheme Claude Code uses for
  `~/.claude/projects/`. It is the project root, not the worktree path, so a
  session started in a worktree sits next to the other sessions of its
  project, matching the sidebar's project folders. Sessions without a project
  use `_no-project`. The slug is lossy (`a-b/c` and `a/b-c` collide); that is
  fine because the folder only groups sessions and the full path stays in the
  session record.
- **Date**: local date of creation.
- **Counter**: per project and day, zero-padded to three digits, wider when
  needed. Allocation is `create_dir` on the next candidate; if the folder
  already exists (another process got there first), take the next number.
  `create_dir` is atomic, so no lock is needed across processes.
- The ID is the folder path relative to `sessions/`. A session keeps its ID
  for life, even if its project is later moved or renamed on disk.

The ID is used as a path, so it has to be validated where it enters from
outside (ACP `session/load`, CLI arguments): only the two components, no
`..`, no absolute paths. Today `chat_file_path` joins the ID unchecked.

Open point: the `/` in the ID. ACP session IDs, lifecycle keys and GPUI
element IDs are plain strings and accept it. If some place turns out to need
a single path component (a file name, a URL segment), use the folder path
there and keep `/` in the ID.

## Externalized tool results

At the persistence boundary, a `SerializedToolExecution` whose `result_json`
serializes to more than a threshold (proposed: 4 KB) is written to
`blobs/<sha256>.json`, and `result_json` in the record is replaced by a
reference:

```json
{"$blob": "9f86d081…", "size": 18392011}
```

On load the reference is resolved before the execution is deserialized.
`agent_core` and the tools don't see any of this.

- **Content-addressed**: a blob is written once and never changed. An
  execution recorded twice (started, then completed) or two identical
  screenshots share one file. The blob is written (and fsynced) before the
  record that refers to it.
- **Threshold**: 4 KB moves 10 of 61 executions in the measured session out
  and with them 99.98 % of the bytes. Small results stay inline, so the
  journal remains readable.
- **Eager loading stays for now.** `render_tool_results_in_messages` and
  `convert_tool_executions_to_ui_data` walk all executions, and the
  `ResourcesTracker` deduplicates file contents across them, so the runtime
  needs every output. Lazy loading is a separate, later step.
- **Format**: blobs hold the `result_json` value as JSON, base64 images
  included, so deserialization doesn't change. Storing images as real image
  files would need changes to the output types and is out of scope.
- Unreferenced blobs (from a crash between blob and record) are removed by
  compaction.

## Append-only journal

`journal.jsonl` holds one record per line. Loading folds the records in
order; for keyed records the last one wins.

```
{"t":"meta",  "id":…, "created_at":…, "config":{…}, "model_config":{…}, "plan_collapsed":false, …}
{"t":"node",  "node":{…MessageNode}}                         upsert by node id
{"t":"exec",  "exec":{…SerializedToolExecution}}             upsert by tool_request.id
{"t":"head",  "name":…, "active_path":[…], "plan":{…}, "active_skills":[…],
              "next_node_id":…, "next_request_id":…, "updated_at":…}
```

Keeping nodes and executions apart is not a problem for this: both are keyed,
and upsert-by-ID is what `apply_checkpoint` does today.

### Writing

- **Agent checkpoint**: one `node` record per changed node, one `exec`
  record per changed execution, one `head` record, written as a single
  `write` under `entry.lock`. `flock` keeps GPUI and ACP processes apart as
  it does now.
- **Other updates**: `update_entry` takes a closure over the whole
  `ChatSession` and has 17 callers in `session/manager.rs` and
  `session/service/lifecycle.rs` (rename, config changes, branch switch,
  lifecycle, …). These mostly touch header fields. They keep their closure,
  run it on the folded state, and the persistence layer compares `meta` and
  `head` before and after and appends whichever changed. These callers don't
  change nodes or executions; a debug assertion checks that, so the 17
  callers don't need typed records.
- `metadata.json` is updated only when the metadata actually changed, not on
  every checkpoint.

### Reading and recovery

- A truncated last line (crash mid-write) is dropped on load.
- A record of unknown type is an error, not something to skip: it means a
  newer version wrote the file.

### Compaction

When the journal is clearly larger than the folded state (say twice), it is
rewritten as one `meta`, one `head`, and one record per live node and
execution, into a temp file that replaces the journal by rename. Natural
moments: loading a session, settling it. Compaction also deletes blobs no
record refers to.

## Migration

Old sessions get new IDs, so migration is a one-time, eager step rather than
lazy on open; otherwise every place that stores an ID would have to handle
both schemes for an unbounded time.

Per session, restartable:

1. Read `chat_<id>.json`, derive the new ID from `config.init_path` and
   `created_at`, allocate the folder under a temporary name.
2. Write the blobs and a compacted journal, move `ui_state`, draft and
   `diag.log` along, rename the folder to its final name.
3. Rewrite the ID in `metadata.json` and `lifecycle.json`, and add
   `old → new` to `sessions/legacy-ids.json`.
4. Move the old file to `sessions/legacy/` instead of deleting it.

`legacy-ids.json` exists for references held outside: ACP clients that
remember session IDs, `--continue <id>`. Lookups by ID consult it when the ID
doesn't resolve to a folder.

Parsing 2.3 GB once takes a while. The migration runs at startup with a log
line per session, holding a lock so a second process waits instead of
migrating in parallel.

## Order of work

1. Folder layout, new session IDs, migration, externalized tool results,
   still writing a full snapshot per checkpoint. The session record drops
   from many MB to a few hundred KB, which takes away most of the cost.
2. Append-only journal with compaction. Removes read-parse-clone-write from
   checkpoints and with it most of the time the `SessionManager` mutex is
   held during a run.
3. Optional: lazy blob loading; UI-only data such as `deleted_contents` kept
   as a blob reference so opening a session doesn't load it.

## Where the pieces live today

- `ChatSession`, `FileSessionPersistence`, `update_entry`,
  `generate_session_id`: `crates/code_assistant_core/src/persistence.rs`
- `atomic_write_json`, `lock_exclusive`:
  `crates/code_assistant_core/src/utils/file_utils.rs`
- Checkpoint deltas: `crates/agent_core/src/persistence.rs`,
  `ToolJournal` in `crates/agent_core/src/execution.rs`,
  `SessionManager::commit_checkpoint` in
  `crates/code_assistant_core/src/session/manager.rs`
- `SerializedToolExecution`: `crates/agent_core/src/types.rs`
- `SessionWatcher`: `crates/code_assistant_core/src/session/watcher.rs`
- Session ID generation is called from `SessionManager` and
  `crates/ui_acp/src/agent.rs`
