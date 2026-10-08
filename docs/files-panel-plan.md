# Files Panel and Line Comments Plan

This document plans two connected features for the GPUI frontend:

1. **Files view** in the right panel: a tree of the project with the
   selected file shown next to it.
2. **Line comments**: in the file viewer and in the Review diffs the user
   selects lines and adds a comment. Comments collect in the composer as a
   compact reference and go to the agent with the next message.

The idea comes from commenting on a plan in other coding agents, extended to
any file in the project.

A list of the files the agent wrote in the session (like "changed files" in
git) is out of scope here; see "Later: files written by the agent".

## Summary

- A third right-panel view, **Files**, next to Review and Browser: the
  project tree on the left, the file on the right. Directories and files are
  read through new `SessionService` methods.
- Line selection already exists in the Review view (`DiffSelection`, the
  "Copy" button). It grows a **Comment** action, and the same selection model
  is reused by the new file viewer.
- Comments are composer state, stored with the draft
  (`DraftAttachment::LineComments`), shown as one chip above the input
  ("3 comments" or a short preview for one), and sent as one structured text
  block the model can read.

## Status

All steps of the order of work are built. File paths in tool cards (the
header of file-editing cards, the file headers of `read_files` and
`search_files` results) open the file in the Files view, at the shown line
where there is one; requests travel over the app-wide
`shared::open_file::OpenFileBus`. A file of another project than the
session's is not opened.

Decisions taken while building:

- **Editor, not a viewer.** The Files view shows the file in
  gpui-component's code editor (`EditorState`: tree-sitter highlighting, line
  numbers, search), so the user can edit it too. Edits save on their own
  about a second after typing stops, when another file is opened, and at
  once on Cmd/Ctrl-S. While the file changed on disk under unsaved edits,
  nothing is saved; a banner offers Reload or Overwrite. Saving goes through
  `SessionService::write_project_file` and keeps the file's encoding.
- **The Review view keeps its diff rows.** Its selection moved into the
  shared `LineSelection` helper; commented rows get a bar at the left edge.
- **Comment card** opens from the selection pill or Cmd/Ctrl-Shift-M (Files
  and Review); Cmd/Ctrl-Enter saves, Escape cancels.
- **Markers.** In the editor, commented lines are tinted with editor
  decorations, which follow edits; in the Review view, rows covered by a
  comment carry a bar. A selection over a commented range edits that
  comment instead of adding one.
- **Paths.** A comment stores the file's absolute path while in the draft;
  the core makes it project-relative when sending. `in_diff` tells where to
  reveal it again (Review or Files).
- **Re-anchoring** happens after a load or save in the editor
  (`line_comments::locate`); diff comments are not re-anchored.
- **Comments on chat messages.** Releasing the mouse over selected text in
  a message shows the selection pill there; its comment button opens the
  card. The comment carries the selected passage (Markdown source) as a
  quote and goes to the model as `<comment on="message">` with a `<quote>`.
  A message containing a pending comment's quote gets a border and a badge
  that opens the comment; the composer's list scrolls to it.
- **Selection pill and floating card everywhere.** In the editor, the
  Review diffs and the chat, a selection shows a small pill (copy, comment)
  at its end, and the comment card floats next to the lines instead of
  docking at the bottom. Positions come from the last frame's layout; when
  they move, one more frame is drawn (`comments::AnchorTracker`). The
  Review rows take presses through a hitbox so the pill above them keeps
  its clicks.
- **Switching views** happens in the title bar (Review | Files | Browser),
  which replaced the panel toggle button: a click opens the panel on that
  view or switches to it; clicking the shown view closes the panel.
- **Watching** uses a new `fs_explorer::watch::TreeWatcher` (the git
  `ChangeWatcher` only covers repositories and does not say which paths
  changed).

Where the code lives: `fs_explorer::browse` and `fs_explorer::watch`;
`code_assistant_core::line_comments` and `session/service/files.rs`; in
`ui_gpui`, `main_screen/right_panel/{files_view, file_viewer, file_filter,
line_selection, comment_editor}.rs`, `review_view/comments.rs`,
`input/comments.rs` and the transcript chip in `blocks/render.rs`.

## Where we are today

- **Right panel.** `RightPanelView` (`ui_gpui/src/main_screen/right_panel/mod.rs`)
  switches between `Review` and, with `browser-panel`, `Browser`, via
  `segmented_switch` in the header. The view is remembered per session
  through `ui_state` (`as_str`/`from_str`). Without `browser-panel` there is no
  header at all.
- **Review view** (`review_view.rs`) stacks changed files per repo with their
  hunks in a virtualized `list()`. Diff bodies are `DiffRows` elements
  (`tool_cards/diff_rows.rs`), which support whole-line selection through
  `RowSelection` (`base_line`, `highlight`, `on_start`/`on_drag`/`on_end`).
  The view keeps one `DiffSelection { file, anchor, head }` and a Copy button.
  Selection indices are flat indices into the file's hunk lines, not file
  line numbers.
- **Diff data.** `PreparedReviewDiff` holds hunks of lines with
  syntax highlights (`tool_cards/diff_syntax.rs`, `diff_prepare.rs`); each
  diff line knows its old/new line numbers (the gutter shows them).
- **Composer.** `InputArea` (`ui_gpui/src/input/mod.rs`) holds text plus
  `Vec<DraftAttachment>`; attachments render as `AttachmentView` tiles and
  persist with the draft (`SessionDraft`, `DraftStore`). On send,
  `utils::content::content_blocks_from` turns each attachment into a content
  block (`Text` → text block, `File` → `"File: name\ncontent"`).
- **`fs_explorer`** reads files with encoding detection
  (`read_file_with_encoding`, `is_text_file`) and knows gitignore
  (`is_path_gitignored`, the explorer's listing).
- **Project root.** `session_effective_path` resolves the session's
  directory, worktree-aware; the Review panel uses it already.
- **No file tree and no plain file viewer** exist in the GPUI frontend.

## UX

### Files view

```
┌ Review | Files | Browser ────────────────────────────────┐
│ 🔍 filter                  │ src/session/service.rs   ⟳  │
│ ▾ crates                   │  1  use anyhow::…            │
│   ▾ code_assistant_core    │  2                           │
│     ▾ src                  │ ...                          │
│       ▸ session            │ 41  pub fn sweep(…) {    💬2 │
│         lib.rs             │ 42      let now = …          │
│ ▸ docs                     │                              │
│   AGENTS.md                │                              │
└──────────────────────────────────────────────────────────┘
```

- **Layout.** Tree on the left (resizable, default ~35% of the panel), viewer
  on the right. The panel is narrow (440 px), so the tree can be collapsed to
  a file-name dropdown above the viewer; the panel width itself stays
  resizable as today.
- **Tree.** A lazy tree of the session's effective project path
  (worktree-aware): directories first, loaded on expand; gitignored entries
  and `.git` hidden by default (a toggle shows them). Expanded directories
  and the open file are remembered per session in `ui_state`.
- **Filter.** A field above the tree matches file names (fuzzy, over a
  background walk of the project that respects gitignore); results replace
  the tree while the field is not empty.
- **Editor.** The file in a code editor (syntax highlighting, line numbers,
  search), editable with auto-save (see Status). Binary files and files over
  the size cap show a notice instead of content.
- **Freshness.** The viewer reloads when the open file changes on disk (a
  watcher on that file, like the Review view's `git::ChangeWatcher`); the
  tree refreshes expanded directories when entries appear or vanish. The
  scroll position is kept across reloads.
- **Open from the transcript.** Clicking a file path in a tool card
  (`read_files`, `write_file`, `edit`, ...) opens the Files view on that
  file and scrolls to the line when the card names one.

### Commenting

- **Select.** Click-drag over lines (gutter or text), shift-click extends,
  exactly as in the Review view today. A selection never spans files.
- **Act.** A small floating bar at the end of the selection offers
  **Comment** and **Copy** (the Review header button moves there).
  Shortcut: `Cmd-Shift-M` (or similar) opens the comment editor for the
  current selection.
- **Edit.** An inline editor opens below the last selected line: multi-line
  input, `Cmd-Enter` saves, `Esc` cancels. Saving adds the comment to the
  composer; the editor closes.
- **Markers.** Commented lines show a marker in the gutter (a speech bubble
  with a count when several comments touch the line). Hovering shows the
  comment; clicking opens it for edit/delete. Markers show in every view
  of that file (file viewer and Review diff).
- **In the composer.** All pending comments show as one chip in the
  attachment row:
  - one comment: `💬 service.rs:41–44 · "this should not unwrap"` (truncated),
  - several: `💬 3 comments in 2 files`.
  Clicking the chip opens a popover listing each comment (file:lines, the
  start of the first selected line, the comment text);
  each entry jumps to the place in the Files/Review view, can be edited or
  removed. The chip's ✕ removes all comments.
- **Send.** Comments go with the next message, even with empty message text
  (the send button is enabled when only comments are pending). After sending
  they are cleared from the composer and the gutter markers disappear (the
  sent message keeps them, see below).
- **In the transcript.** The user message shows the same chip, collapsed;
  expanding it lists the comments with their code excerpts.

## Data model

### Comment (core, `code_assistant_core::line_comments`)

```rust
pub struct LineComment {
    pub id: u64,          // unique within the draft, for edit/delete
    pub file: PathBuf,    // absolute in the draft; project-relative once sent
    pub start_line: usize, // 1-based, inclusive
    pub end_line: usize,
    pub old_side: bool,   // the lines count in the old file (deleted rows)
    pub in_diff: bool,    // made in the Review view
    pub excerpt: String,  // the selected lines, as shown
    pub text: String,     // the user's comment
}

pub enum DraftAttachment {
    // ...existing variants...
    #[serde(rename = "line_comments")]
    LineComments { comments: Vec<LineComment> },
}
```

- One `LineComments` attachment per draft holds every comment; the composer
  keeps it last in the attachment list. It persists with the draft like the
  other attachments, so comments survive switching sessions and restarts.
- `excerpt` is captured when the comment is made. Line numbers alone go stale
  as soon as the agent edits the file again; the excerpt lets the model find
  the place anyway and lets the transcript render the comment without the
  file.
- **Diff lines map to file lines.** The Review selection is in flat hunk
  indices; on Comment the view maps them to new-side line numbers (each
  prepared diff line knows its numbers). A selection made only of deleted
  lines uses the old side (`old_side`). A mixed selection is stored with the
  new-side range plus the full excerpt including `-` lines.

### Sending

`content_blocks_from` renders `LineComments` as one text block:

```
<line-comments>
<comment path="crates/foo/src/lib.rs" lines="41-44">
<code>
    let x = y.unwrap();
    ...
</code>
this should not unwrap; return the error
</comment>
...
</line-comments>
```

- Each comment carries its line range **and** the selected lines
  themselves, so the agent sees the code it is about without a file read and
  can find the place again after line numbers shifted.
- A fixed tag lets the GPUI transcript recognize the block and render it as
  the collapsed chip instead of raw text (the same block is what the model
  sees, so stored sessions need no extra field).
- Paths are relative to the session's project root when the file lies inside
  it, so the model can pass them to its file tools; otherwise absolute.
- Excerpts are capped (e.g. 40 lines per comment: the first and last lines
  kept, the middle elided with a `… N lines …` marker) to keep a large
  selection from flooding the context.
- The ACP and terminal frontends render the block as text; they get no
  commenting UI in this plan.

## File access (core)

The viewer reads through the core, not `std::fs` in the UI, so sandbox and
worktree rules stay in one place:

- `SessionService::list_dir(session_id, rel_dir) -> Vec<DirEntry>`
  (name, kind, gitignored), backed by `fs_explorer` (which already knows
  gitignore). The root is the session's `session_effective_path`, so a
  worktree switch moves the tree with it.
- `SessionService::read_file_for_view(session_id, rel_path) -> FileView`
  with the text (via `fs_explorer::read_file_with_encoding`), a size cap with
  a "file too large" state, and a binary flag (images could reuse
  `shared::image` later).
- `SessionService::find_files(session_id, query) -> Vec<PathBuf>` for the
  filter, capped in result count.
- Paths are checked to stay inside the root (no `..` escapes, no symlinks
  out of it).
- Run on the io side (`call_io`, `spawn_blocking`) like the Review scans, so
  a big directory never blocks the session's command worker.

## UI (ui_gpui)

- **`RightPanelView::Files`**, persisted as `"files"`. The header with the
  segmented switch is shown whenever there is more than one view, also
  without `browser-panel`.
- **`files_view.rs`** in `main_screen/right_panel/`: a `FilesView` entity
  with the tree (a `list()` over flat rows of the expanded entries, like
  `review_rows.rs`) and a `FileViewer` entity. Data comes in the way the
  Review view gets it: commands in `app/commands.rs` call the service and
  hand results to the view.
- **`FileViewer`**: content split into chunks of rows, rendered with
  `DiffRows` without diff colors (so selection, wrapping and painting come for
  free and stay fast; see `docs/frame-profiling.md`), highlighted with the
  existing syntax machinery. Highlighting runs off the main thread on load,
  as for diffs.
- **Shared selection and comments.** Pull the selection logic out of
  `ReviewView` into a small `LineSelection` helper (anchor/head, drag state,
  mapping to file lines through a per-view callback) used by both views. The
  floating action bar, inline comment editor and gutter markers are shared
  components too.
- **Comment store in the UI.** The pending comments of the shown session live
  in the composer (`InputArea`, as the `LineComments` attachment). Views read
  them through a `PendingComments` global (or an entity owned by the main
  screen) for markers, and add/edit/remove through it; the input area
  re-renders its chip from the same data and the draft is saved as usual.
- **Composer chip.** A new `AttachmentView` variant for `LineComments` with
  the popover. `InputArea::is_empty` and the send-enabled check count
  comments as content.
- **Transcript.** The user-message renderer detects a text block starting
  with `<line-comments>` and renders the collapsed chip.
- **Jump to a comment.** From the chip popover: switch the panel to Files (or
  Review for diff comments), open the file and scroll to the lines. If the
  lines no longer match the excerpt, search the file for the excerpt and use
  the first match; otherwise show the stored range with a "file changed"
  hint.

## Edge cases

- **File changed after commenting.** Markers are placed by line number; when
  the file reloads, re-anchor each comment by its excerpt (exact match first,
  then nearest occurrence to the old line). Unplaceable comments stay in the
  composer, marked "outdated", and are still sent with their excerpt.
- **Same lines commented twice.** Allowed; the gutter count shows it.
- **Session switch.** Comments belong to the session's draft; switching
  sessions shows that session's comments and markers.
- **Message edit mode.** Comments attach to the edited message like other
  attachments.
- **Queued messages** (agent busy): the comments go with the queued message;
  `queue_user_message` takes only text today, so either extend it to
  attachments or keep the Send button for comments disabled while the agent
  runs. Recommended: extend it.
- **Session without a project path**, or a path that no longer exists: the
  Files view shows an empty state.
- **Large files**: viewer caps (e.g. 2 MB / 50k lines) with a notice;
  commenting still works within what is shown.
- **Large directories** (`node_modules`, `target`): gitignored, so hidden by
  default; when shown, listings are capped with a "N more" row.

## Order of work

1. **Files view, read-only.** `list_dir` / `read_file_for_view` service
   methods with tests; `RightPanelView::Files`; the right-panel header
   without `browser-panel`; lazy tree and the file viewer (highlighted,
   virtualized), remembered per session.
2. **Freshness and filter.** Watcher on the open file and expanded
   directories; `find_files` and the filter field.
3. **Selection and comments in the file viewer.** Shared `LineSelection`
   helper (pulled out of `ReviewView`), `LineComment`,
   `DraftAttachment::LineComments`, rendering in `content_blocks_from`,
   composer chip and popover, floating Comment/Copy bar, inline editor.
4. **Comments in the Review diffs**, with the diff-to-file line mapping.
5. **Gutter markers and re-anchoring**; transcript chip; jump to a comment.
6. Clicking file paths in tool cards opens the Files view.

Tests: core tests for `list_dir` / `read_file_for_view` (gitignore, size cap,
binary, path escapes), the comment block rendering and re-anchoring; GPUI
tests with `MainScreenTest` for opening a file from the tree, selecting
lines, adding a comment, the chip text for one/several comments, and that
sending clears them.

## Later: files written by the agent

A second section above the tree, *Written by agent*, listing the files the
agent wrote in the session (newest first, `A`/`M`/`D` like the Review view),
with a *Changes* toggle in the viewer that diffs against the content before
the session's first write. Sketch for when it is picked up:

- The core derives the list from the active path of the transcript
  (successful calls of tools tagged e.g. `writes_files`: `write_file`,
  `edit`, `replace_in_file`, `delete_files`), so branches and message edits
  are handled for free; sub-agent writes included.
- `SessionService::written_files`, a `written_files` field in
  `SessionSnapshot`, and a `UiEvent::WrittenFilesChanged` on each completed
  write.
- Baselines for the *Changes* diff captured by the tools before the first
  write, stored beside the session, capped by size.

## Open questions

- Should a sent comment stay visible as a marker on the file (a review
  history per session), or disappear once sent? The plan says disappear.
- Should the agent be able to reply to a comment (threaded, like PR reviews),
  e.g. through a tool that resolves comments? Out of scope for now.
- Should the tree also offer opening a file in the system editor
  (`Cmd-click`)? Cheap, but outside the review flow.
- Separate shortcut for "comment" vs. reusing `Cmd-Enter` in the panel.
