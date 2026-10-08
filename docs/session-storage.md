# Session storage: folders, blobs and an append-only journal

Sessions used to be one JSON file each (`sessions/chat_<id>.json`), and every
agent checkpoint rewrote that file completely. With browser screenshots and
large tool outputs this got expensive fast. This document describes the
storage that replaced it, where writes are proportional to what changed.

Code: `crates/code_assistant_core/src/persistence/` — `layout.rs` (paths and
IDs), `blobs.rs`, `journal.rs`, `migration.rs`, and `FileSessionPersistence`
in `mod.rs`.

## Why

### Where the bytes were

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

The sessions directory held 1148 session files, 2.3 GB in total; five of
them between 150 and 400 MB.

### What a checkpoint cost

`AgentRuntime::checkpoint` already produced a delta: `SessionCheckpoint`
carries only `changed_nodes` and `changed_executions` plus the small
always-current fields (`active_path`, `plan`, counters). The persistence
layer turned it back into a full rewrite: read and parse the whole file,
merge, clone, write it completely (pretty-printed, temp file + rename), then
read and rewrite `metadata.json`.

The agent loop checkpoints several times per step, so the total cost grew
quadratically with session length. One GPUI run of about an hour with a
browser session that grew to 18 MB made macOS file a disk-writes diagnostic
report: 8.6 GB written, mostly from `SessionManager::commit_checkpoint`, all
while the global `SessionManager` mutex was held.

## Layout

One folder per session, named after the session ID:

```
sessions/
  metadata.json                      global index
  lifecycle.json                     visits and settlement
  legacy-ids.json                    old → new IDs, from the migration
  legacy/                            the old session files
  -Users-me-workspace-code-assistant/
    2026-10-07-001/
      journal.jsonl                  the session record
      blobs/<sha256>.json            large tool results, immutable
      ui_state.json                  GPUI view state
      draft.json                     unsent composer content
      agent.lock                     held while an agent runs
      entry.lock                     serializes record updates
    2026-10-07-002/
      ...
```

`SessionLayout` owns every path. The watcher watches `sessions/`
recursively and maps changed paths back to sessions with
`SessionLayout::classify`; only `journal.jsonl`, `agent.lock`,
`metadata.json` and `lifecycle.json` matter to it. Deleting a session removes
its folder. Drafts and UI state are not written for a session whose folder
is gone, so a late debounced save doesn't bring a deleted session back.

## Session IDs

IDs are readable and say where a session lives, without reading
`metadata.json`:

```
<project-slug>/<YYYY-MM-DD>-<NNN>
-Users-me-workspace-code-assistant/2026-10-07-003
```

- **Project slug**: the session's project root (`SessionConfig::init_path`)
  with every character other than ASCII letters, digits, `_` and `-`
  replaced by `-`, the scheme Claude Code uses for `~/.claude/projects/`. It
  is the project root, not the worktree path, so a session started in a
  worktree sits next to the other sessions of its project. Sessions without
  a project use `_no-project`. The slug is lossy (`a-b/c` and `a/b-c`
  collide); the folder only groups sessions, the full path stays in the
  record.
- **Date**: local date of creation.
- **Counter**: per project and day, after the highest existing number,
  zero-padded to three digits. Allocation is `create_dir`: if the folder
  already exists (another process got there first), the next number is
  taken, so no lock is needed across processes. ACP hands out the ID in
  `session/new` but creates the session on the first prompt; allocating
  reserves the folder in between.
- A session keeps its ID for life, even if its project is later moved.

IDs become paths, so `validate_session_id` rejects anything but one or two
plain `/`-separated components (no `.`, `..`, absolute paths or
backslashes). IDs supplied from elsewhere (tests, embedders, ACP) may be any
such path.

## Externalized tool results

A `SerializedToolExecution` whose `result_json` serializes to more than 4 KB
is written to `blobs/<sha256>.json`, and the record keeps a reference:

```json
{"$blob": "9f86d081…", "size": 18392011}
```

`load_chat_session` resolves the references before the executions are
deserialized, so `agent_core` and the tools don't see any of this. Showing
a session doesn't read them at all (see [Reading on
demand](#reading-tool-results-on-demand)).

- **Content-addressed**: a blob is written once and never changed, so saving
  doesn't rewrite stored results, and identical results (the same
  screenshot, a re-recorded execution) share a file.
- **Threshold**: 4 KB moves 10 of 61 executions in the measured session out
  and with them 99.98 % of the bytes. Small results stay inline.
- **Durability**: a blob is handed to the drive with a plain `fsync`, without
  the drive-cache flush (`F_FULLFSYNC`) that takes milliseconds per call on
  macOS. The journal record referring to it is written afterwards with the
  full flush, which makes both durable in order.
- Blobs nothing refers to any more are deleted during compaction.

## Append-only journal

`journal.jsonl` holds one record per line. Loading folds them in order; the
last `header` wins, `node` and `exec` records replace earlier ones with the
same id:

```
{"t":"header", "session":{…the session without nodes and executions…}}
{"t":"node",   "node":{…MessageNode…}}
{"t":"exec",   "exec":{…SerializedToolExecution, large result as $blob…}}
```

### Writing

- **Agent checkpoint** (`FileSessionPersistence::commit_checkpoint`): folds
  the journal without resolving blobs, applies the checkpoint, and appends
  the changed nodes and executions plus a header if the header changed — one
  synced write under `entry.lock`. Stored tool results are neither read nor
  written. The run gets the new metadata back for its notification.
- **Other updates** (`update_entry`): the closure sees the whole
  session, with its tool results unresolved. Afterwards `journal::diff` compares before and after and
  appends only what differs.
- `metadata.json` is written only when the session's entry changed.

### Reading and recovery

Every append ends with a newline. A file that doesn't end in one was cut off
mid-write, and its partial last line is dropped. Any other line that doesn't
parse, including a record of unknown type, is an error: a newer version may
have written it.

### Compaction

When the journal holds more than twice the records a snapshot would (plus a
small allowance), it is rewritten as a snapshot (temp file + rename) and
unreferenced blobs are deleted. The check runs after each append, from the
record counts both write paths already have.

## Migration

`migrate_legacy_sessions` runs at startup, before the frontends touch the
session store. It is cheap when there are no flat session files, and holds
`sessions/migration.lock` otherwise so a second process waits. It reports
`MigrationProgress` as it goes: GPUI runs it on the backend thread and shows
a small window with a progress bar instead of the main window, which opens
when the migration is done; the terminal UI and ACP print the progress to
stderr. The MCP server mode doesn't migrate.

Old sessions get new IDs, so everything that stores IDs is rewritten:
`metadata.json`, `lifecycle.json`, and the `session:<id>` owner keys in
`goals.json` and `waits.json`. UI state, drafts (from the old drafts
directory, with their `session_id` rewritten) and `diag.log` move into the
session folder. The phases make the run restartable at any point:

1. Read each old session's project and creation date (a partial parse).
2. Allocate the new IDs in creation order (by the timestamp encoded in the
   old ID) and write `legacy-ids.json`.
3. Write the new sessions, four at a time, and move their side files.
4. Rewrite the IDs in the index, the lifecycles and the goal stores.
5. Move the old session files to `sessions/legacy/`.

A rerun takes the recorded IDs, skips sessions that already have a journal,
and repeats the idempotent rest. Until the IDs are recorded, each folder
reserved in phase 2 holds a `migration-reservation` file naming its old
session, and a rerun takes those folders over instead of numbering past
them. An empty folder alone wouldn't do: it can also be an ACP reservation of
a running instance. Sessions that fail to parse or have an agent
of an older version running stay in place and are reported.

On the 1148 sessions above the migration took 16 s, every session loaded
afterwards, and loading all of them took 13 s.

`legacy-ids.json` is a record for looking up old IDs, not an alias table:
ACP clients see the new IDs through `session/list`, and a client that sends
an old ID to `session/load` can't be redirected, because it would keep using
that ID for every later request.

## Reading tool results on demand

Reading every blob made switching sessions slower than with the single
session file: opening a blob file took about 0.8 ms even with a warm cache
(on a machine whose endpoint protection scans files as they are opened),
and cold up to several milliseconds, on top of parsing, deserializing and
rendering the result. So showing a session reads none of them:

- **Resident sessions** (`SessionInstance::session`, and what
  `update_entry` returns) keep large tool results as blob references
  (`load_chat_session_unresolved`, `is_blob_reference`). A resident
  session is read again only when its journal changed
  (`journal_version`: inode, length, modification time); revisiting an
  unchanged session reads nothing.
- **Snapshots and transcripts** (`convert_tool_executions_to_ui_data`)
  leave out the output of a successful stored result and mark it
  `ToolResultData::output_deferred`. Status and duration come from the
  conversation's `tool_result` blocks, which record `is_error` and the
  timestamps. Failures and results without a `tool_result` block are read,
  their output explains them.
- **On demand**: `SessionService::load_tool_output` returns one result
  complete. It takes the record from the manager and reads the blob after
  letting go of it. GPUI asks when the block shows the output: cards that
  are expanded right away, inline blocks when expanded. The answer arrives
  as a tool status update.
- **Rendering for the UI doesn't deduplicate**: only `read_files::render`
  (the LLM's view) uses the `ResourcesTracker`, so each result renders on
  its own the same as in sequence.
- **Followers get everything**: the watcher refresh appends only new
  executions, with their outputs (ACP replays them).
- **Agent runs** resolve every result they start from.

Measured from the click in the GPUI sidebar to the end of the first frame
showing the session (release build, fresh APFS clone of the data for
"cold", the second visit for "warm"; milliseconds):

| Session                         | single file cold / warm | all blobs cold / warm | on demand cold / warm |
|---------------------------------|-------------------------|-----------------------|-----------------------|
| 500 KB, 17 blobs                | 239 / 128               | 298 / 137             | 182 / 129             |
| 2.5 MB, 50 blobs                | 304 / 261               | 541 / 302             | 251 / 198             |
| 16 MB, 217 blobs                | 780 / 752               | 1775 / 874            | 349 / 307             |
| 118 MB, 123 blobs (1)           | 1369 / 1316             | 1441 / 1328           | 257 / 306             |
| 394 MB, 237 MB `delete_files`   | 1040 / 720              |                       | 168 / 131             |

(1) The app showed this session at start, so its first switch was already
warm.

Besides the blobs, the GPUI frontend spent up to 800 ms applying the tool
results of a large session: each result went to every message container,
copying its output once per container. They now apply in one pass by
tool ID. What remains is converting the messages (20 to 70 ms) and GPUI's
first layout of all rows (60 to 160 ms).

## Possible next steps

- UI-only data such as `deleted_contents` kept out of the result the
  runtime loads.
- Images stored as image files instead of base64 inside JSON blobs.
