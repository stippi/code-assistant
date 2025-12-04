# Remove Working Memory UI Implementation Plan

## Overview

This plan outlines the removal of the Working Memory UI from the GPUI version of the application, while preserving the event-driven notification system for file and directory operations. The goal is to reduce coupling between the tool registry/execution module and the agent code, enabling the tools module to eventually be extracted into its own crate.

## Current State Analysis

### WorkingMemory Usage

The `WorkingMemory` struct currently serves multiple purposes:

1. **UI Display**: Showing loaded resources and file trees in the GPUI sidebar
2. **Session Persistence**: Stored in `SessionState` and `ChatSession` for session recovery
3. **System Prompt Generation**: Provides `file_trees` and `available_projects` for the system prompt
4. **Resource Tracking in Tools**: Tools update `loaded_resources` when files are read/written

### Components Using WorkingMemory

| Component | Location | Usage |
|-----------|----------|-------|
| `MemoryView` | `ui/gpui/memory.rs` | UI display (to be removed) |
| `Agent` | `agent/runner.rs` | System prompt generation, memory management |
| `SessionState` | `session/mod.rs` | Persistence structure |
| `ChatSession` | `persistence.rs` | Persistence to disk |
| `ToolContext` | `tools/core/tool.rs` | Passed to tools for resource tracking |
| Tools | `tools/impls/*.rs` | Update `loaded_resources`, `file_trees`, `expanded_directories` |
| `UiEvent::UpdateMemory` | `ui/ui_events.rs` | Event to update UI (to be removed) |
| `AppState` | `ui/terminal/state.rs` | Terminal UI state (keeps field, unused) |

## Design Goals

1. **Remove the Working Memory UI panel** from GPUI entirely
2. **Preserve event emission** when files are loaded or directories listed
3. **Stop tracking visited directories** in Explorer instances
4. **Remove the `WorkingMemory` struct** and its usage across the codebase
5. **Reduce coupling** so tools module could become an independent crate

## Implementation Phases

### Phase 1: Add Resource Events to UiEvent (Low Risk)

**Goal**: Extend the existing `UiEvent` enum to include resource operation notifications. This follows the established pattern where `UiEvent` carries state updates to the UI.

#### Design Rationale

The codebase already has two event mechanisms:
- `DisplayFragment` - for real-time streaming content (text, tool invocations)
- `UiEvent` - for discrete state updates (session changes, tool status, memory updates)

Resource operations are discrete state changes, so they belong in `UiEvent`. This avoids introducing new traits or coupling mechanisms.

#### Tasks

1. **Add new variants to `UiEvent`** in `ui/ui_events.rs`:
   ```rust
   /// A file was loaded/read by a tool
   ResourceLoaded {
       project: String,
       path: PathBuf,
   },
   /// A file was written/modified by a tool
   ResourceWritten {
       project: String,
       path: PathBuf,
   },
   /// A directory was listed by a tool
   DirectoryListed {
       project: String,
       path: PathBuf,
   },
   /// A file was deleted by a tool
   ResourceDeleted {
       project: String,
       path: PathBuf,
   },
   ```

2. **Handle new events in GPUI** (`ui/gpui/mod.rs`):
   - Add match arms that log the events (for now)
   - These can be extended later for features like "follow mode"

3. **Handle new events in Terminal UI** (`ui/terminal/ui.rs`):
   - Add match arms that log the events

### Phase 2: Update Tool Implementations (Medium Risk)

**Goal**: Modify tools to emit `UiEvent`s via `ToolContext.ui` instead of updating WorkingMemory directly.

#### Tasks per Tool

| Tool | Current WorkingMemory Usage | New UiEvent |
|------|---------------------------|-------------|
| `read_files` | Insert into `loaded_resources` | `ResourceLoaded` |
| `write_file` | Insert into `loaded_resources` | `ResourceWritten` |
| `edit` | Insert into `loaded_resources` | `ResourceWritten` |
| `replace_in_file` | Insert into `loaded_resources` | `ResourceWritten` |
| `delete_files` | Could remove from `loaded_resources` | `ResourceDeleted` |
| `list_files` | Update `file_trees`, `expanded_directories`, `available_projects` | `DirectoryListed` |

#### Detailed Changes

1. **read_files.rs** (~5 lines):
   - Remove: `working_memory.loaded_resources.insert(...)`
   - Add: Send `UiEvent::ResourceLoaded` via `context.ui`

2. **write_file.rs** (~5 lines):
   - Remove: `working_memory.loaded_resources.insert(...)`
   - Add: Send `UiEvent::ResourceWritten` via `context.ui`

3. **edit.rs** (~5 lines):
   - Same pattern as write_file

4. **replace_in_file.rs** (~5 lines):
   - Same pattern as edit

5. **delete_files.rs** (~5 lines):
   - Add: Send `UiEvent::ResourceDeleted` via `context.ui`
   - (Currently doesn't update WorkingMemory)

6. **list_files.rs** (~20 lines):
   - Remove: All `file_trees`, `expanded_directories`, `available_projects` updates
   - Add: Send `UiEvent::DirectoryListed` via `context.ui`

#### Note on Event Emission

Tools already have access to `context.ui: Option<&'a dyn UserInterface>` for streaming output. We'll reuse this for resource events. The pattern is:

```rust
if let Some(ui) = context.ui {
    let _ = ui.send_event(UiEvent::ResourceLoaded {
        project: input.project.clone(),
        path: path.clone(),
    }).await;
}
```

Since `send_event` is async, we'll need to handle this appropriately (tools are already async).

### Phase 3: Update ToolContext and Agent (Medium Risk)

**Goal**: Remove `working_memory` from `ToolContext` and `Agent`. Tools already have `ui` access for events.

#### Tasks

1. **Update `ToolContext`** in `tools/core/tool.rs`:
   - Remove: `working_memory: Option<&'a mut WorkingMemory>`
   - The `ui` field already exists and will be used for events

2. **Update `execute_tool()`** in `agent/runner.rs`:
   - Remove: `working_memory: Some(&mut self.working_memory)` from context construction

3. **Remove `working_memory` field** from `Agent` struct

4. **Update `init_working_memory()`** and `init_working_memory_projects()`:
   - These methods currently build file trees for system prompt
   - Keep the file tree building logic but store trees differently (see Phase 4)

### Phase 4: Preserve System Prompt Information (Medium Risk)

**Goal**: Keep the ability to show available projects and initial file tree in system prompts without WorkingMemory.

#### Tasks

1. **Add dedicated fields to `Agent`** for system prompt data:
   ```rust
   struct Agent {
       // ... existing fields ...
       available_projects: Vec<String>,
       initial_file_tree: Option<FileTreeEntry>, // For initial project only
       initial_project: String,
   }
   ```

2. **Update `get_system_prompt()`** to use these fields instead of `self.working_memory.available_projects` etc.

3. **Update `init_working_memory_projects()`** → rename to `init_projects()`:
   - Still queries ProjectManager for available projects
   - Still creates initial file tree for the initial project
   - Stores in Agent fields instead of WorkingMemory

### Phase 5: Remove GPUI Working Memory UI (Low Risk)

**Goal**: Remove all UI components related to Working Memory display.

#### Tasks

1. **Delete `ui/gpui/memory.rs`** entirely

2. **Update `ui/gpui/mod.rs`**:
   - Remove `mod memory;`
   - Remove `pub use memory::MemoryView;`
   - Remove `working_memory` field from `Gpui` struct
   - Remove `UiEvent::UpdateMemory` handling

3. **Update `ui/gpui/root.rs`**:
   - Remove `memory_view: Entity<MemoryView>` field
   - Remove memory sidebar toggle button
   - Remove memory sidebar rendering (both expanded and collapsed states)
   - Remove `memory_collapsed` state field

4. **Update `ui/ui_events.rs`**:
   - Remove `UpdateMemory { memory: WorkingMemory }` variant

5. **Update `ui/terminal/state.rs`**:
   - Remove `working_memory: Option<WorkingMemory>` field (or keep as dead code if minimal impact)

### Phase 6: Remove WorkingMemory from Persistence (Medium Risk)

**Goal**: Stop persisting WorkingMemory in sessions since it's no longer used.

#### Tasks

1. **Update `SessionState` struct** in `session/mod.rs`:
   - Remove `working_memory: WorkingMemory` field

2. **Update `ChatSession` struct** in `persistence.rs`:
   - Remove `working_memory: WorkingMemory` field
   - Note: Existing session files will have this field; handle gracefully during deserialization

3. **Update `FileStatePersistence::save_agent_state()`** in `agent/persistence.rs`:
   - Remove working_memory from saved state

4. **Update `Agent::load_from_session_state()`**:
   - Remove `self.working_memory = session_state.working_memory;`

5. **Update test fixtures** in `tests/mocks.rs` and other test files

### Phase 7: Remove Explorer Directory Tracking (Low Risk)

**Goal**: Remove `expanded_paths` from Explorer since it's no longer used for WorkingMemory.

The `expanded_paths` field is a remnant from an earlier version where the full directory tree (with expanded/collapsed state) was shown on every turn. Now that each `list_files` call returns exactly what the model requested, this tracking is unnecessary.

#### Tasks

1. **Update `Explorer` struct** in `fs_explorer/src/explorer.rs`:
   - Remove: `expanded_paths: HashSet<PathBuf>` field
   - Simplify `expand_directory()` to only consider `max_depth`, not `expanded_paths`
   - The condition `current_depth < max_depth || self.expanded_paths.contains(path)` becomes just `current_depth < max_depth`

### Phase 8: Delete WorkingMemory Type (Low Risk)

**Goal**: Final cleanup - remove the WorkingMemory type definition.

#### Tasks

1. **Update `types.rs`**:
   - Remove `WorkingMemory` struct
   - Remove `LoadedResource` enum (unless used elsewhere)
   - Remove `tuple_key_map` module

2. **Search and fix any remaining references**

## Migration Notes

### Backward Compatibility

- Existing session files contain `working_memory` field
- Use `#[serde(default)]` on new structs to allow loading old sessions
- Old `working_memory` data in session files will be ignored

### Test Updates Required

- `tests/format_on_save_tests.rs`: Update tests that verify WorkingMemory updates
- `tests/mocks.rs`: Remove `ToolTestFixture::with_working_memory()` method
- `agent/tests.rs`: Update agent tests that check WorkingMemory state
- Tool-specific tests: Update to verify events instead of WorkingMemory mutations

## Resolved Questions

1. **Event mechanism**: Use existing `UiEvent` enum with new variants. This follows the established pattern and avoids new traits/coupling.

2. **Explorer expanded_paths**: Remove. It's a remnant of an earlier architecture where file trees accumulated across turns.

3. **Terminal UI working_memory field**: Remove for cleanliness.

4. **Event granularity**: Just path in events. The full tree/content is in tool output already.

5. **Events to UI**: Events flow to UI for future extensibility (e.g., ACP follow mode).

## Risk Assessment

| Phase | Risk Level | Reason |
|-------|------------|--------|
| 1 | Low | Adding new types, no breaking changes |
| 2 | Medium | Modifying tool implementations |
| 3 | Medium | Agent structure changes |
| 4 | Medium | System prompt generation changes |
| 5 | Low | UI removal, isolated changes |
| 6 | Medium | Persistence schema changes |
| 7 | Low | Internal Explorer cleanup |
| 8 | Low | Final type deletion |

## Success Criteria

1. GPUI application runs without Working Memory sidebar
2. All existing tests pass (with necessary updates)
3. Session persistence works (loading old sessions, saving new sessions)
4. System prompt still contains project information and initial file tree
5. Tools emit appropriate events when operating on files
6. No references to `WorkingMemory` remain in codebase
