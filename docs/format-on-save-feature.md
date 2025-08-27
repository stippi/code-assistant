# Format-on-Save Feature Implementation

This document describes the comprehensive implementation of the format-on-save feature that automatically formats files after they are modified by the code assistant, while maintaining consistency with the LLM's mental model of the code.

## Overview & The Core Problem

When an LLM generates code that gets auto-formatted after saving, the LLM's mental model of the file becomes inconsistent with the actual file contents. This can cause subsequent edits to fail because the LLM expects the code to be in its original (unformatted) state.

**The Solution**:
1. Run format commands after file modifications
2. Update the tool parameters in the message history to match the formatted output
3. Make it appear to the LLM that it generated perfectly formatted code from the beginning

## Configuration

Extended the `Project` struct in `crates/code_assistant/src/types.rs` to include:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Project {
    pub path: PathBuf,
    #[serde(default)]
    pub format_on_save: Option<HashMap<String, String>>,
}
```

Example project configuration in `~/.config/code-assistant/projects.json`:

```json
{
  "my-rust-project": {
    "path": "/path/to/my/rust/project",
    "format_on_save": {
      "*.rs": "cargo fmt",
      "*.toml": "taplo format"
    }
  },
  "my-js-project": {
    "path": "/path/to/my/js/project",
    "format_on_save": {
      "*.js": "prettier --write",
      "*.ts": "prettier --write",
      "*.json": "prettier --write"
    }
  }
}
```

## Implementation Progress

### ✅ Phase 1: Enhanced File Updater (COMPLETED)

**File**: `crates/code_assistant/src/utils/file_updater.rs`

**Key Components**:
- `MatchRange` struct: Represents individual matches with position info
- `FileUpdaterError`: Extended with `OverlappingMatches` and `AdjacentMatches` variants
- `find_replacement_matches()`: Finds all matches and detects conflicts
- `apply_matches()`: Applies replacements using pre-found matches
- Conflict detection for overlapping/adjacent matches
- Backward compatibility maintained through existing `apply_replacements_normalized()`

**Features**:
- Split functionality between finding matches and applying them
- Detects overlapping matches (returns error) and adjacent matches (returns flag)
- All existing tests pass, new tests added for stable range functionality

### ✅ Phase 2: Stable Range Extraction (COMPLETED)

**File**: `crates/code_assistant/src/utils/file_updater.rs`

**Key Components**:
- `StableRange` struct: Represents unchanged content between matches
- `extract_stable_ranges()`: Identifies content ranges that should remain unchanged after formatting
- `reconstruct_formatted_replacements()`: Attempts to reconstruct formatted replacement parameters using stable ranges as anchors

**Features**:
- Graceful failure: Returns `None` if reconstruction fails (allows fallback to leaving replacements unchanged in the LLM history)
- Handles single matches (non-`replace_all` cases)
- Uses content normalization for consistent matching
- Comprehensive test coverage

**Key Insight**: Stable ranges can't contain content that gets modified by formatting (like indentation). Works best when stable ranges contain truly stable content like keywords and structural elements.

### ✅ Phase 3: Tool Formatter System (COMPLETED)

**File**: `crates/code_assistant/src/tools/formatter.rs`

**Key Components**:
- `ToolFormatter` trait: Formats tool requests into string representation
- `NativeFormatter`: Handles JSON-based function calls
- `XmlFormatter`: Handles `<tool:name>` syntax
- `CaretFormatter`: Handles `^^^tool_name` syntax
- `get_formatter()`: Factory function for syntax-specific formatters

**Features**:
- Supports all three tool syntaxes used by the code assistant
- Clean abstraction for regenerating tool blocks
- Used for updating message history after formatting

### ✅ Tool Trait Extension (COMPLETED)

**Files**:
- `crates/code_assistant/src/tools/core/tool.rs`
- `crates/code_assistant/src/tools/core/dyn_tool.rs`
- `crates/code_assistant/src/agent/runner.rs`

**Changes**:
- Modified `Tool::execute()` to take input as mutable Self::Input reference, allowing to return modified input parameters
- Updated `DynTool::invoke()` to take param as mutable reference, allowing to return modified parameter if the tool modified input
- Updated agent runner to detect input parameter changes and update message history
- All tool implementations updated

**Benefits**:
- Clean detection of parameter changes during tool execution
- Automatic message history synchronization
- Maintains backward compatibility

### ⚠️ Phase 3: Explorer Integration (PARTIALLY COMPLETED - BLOCKED)

**Issue Encountered**: Async methods in traits make them not dyn-compatible (can't be used as trait objects like `Box<dyn CodeExplorer>`). The codebase extensively uses trait objects for `CodeExplorer`.

**Files Affected**:
- `crates/code_assistant/src/types.rs` (CodeExplorer trait)
- `crates/code_assistant/src/explorer.rs` (Explorer implementation)
- `crates/code_assistant/src/tests/mocks.rs` (MockExplorer implementation)

**What Was Attempted**:
- Added `apply_replacements_with_formatting()` async method to `CodeExplorer` trait
- Implemented format-aware replacement logic in `Explorer`
- Added mock implementation for testing

**Current State**: Code doesn't compile due to dyn-compatibility issues.

## Technical Architecture

### Format-Aware Workflow
1. **File Modification Detection**: Tools detect when files are modified
2. **Pattern Matching**: Check if modified file matches format-on-save patterns
3. **Stable Range Extraction**: Identify unchanged content between replacements
4. **Format Command Execution**: Run format command in project directory
5. **Parameter Reconstruction**: Use stable ranges to extract formatted replacement text
6. **Message History Update**: Update tool parameters in LLM message history

### Conflict Handling
- **Overlapping matches**: Return error (cannot be safely formatted)
- **Adjacent matches**: Skip parameter reconstruction, but still apply formatting
- **Formatting failures**: Preserve original content, skip parameter updates
- **Reconstruction failures**: Apply formatting but don't update LLM parameters

## Remaining Work

### 1. Solve Dyn-Compatibility Issue
**Priority**: High
**Options to explore**:
- Move format-on-save logic directly into individual tools (edit, replace_in_file, write_file)
- Create a separate formatting service that tools can use
- Use enum dispatch instead of trait objects
- Split CodeExplorer into sync and async traits

### 2. Tool Integration
**Files to modify**:
- `crates/code_assistant/src/tools/impls/edit.rs`
- `crates/code_assistant/src/tools/impls/replace_in_file.rs`
- `crates/code_assistant/src/tools/impls/write_file.rs`

**Tasks**:
- Integrate format-on-save detection using project configuration
- Use stable range extraction for parameter reconstruction
- Handle edge cases and error conditions
- Update tool input parameters when formatting succeeds

### 3. Message History Updates for XML/Caret Syntax
**Priority**: Medium
**Current limitation**: Only handles native (JSON) tool syntax properly
**Need**: Store text offsets during parsing to enable precise replacement of tool blocks in assistant messages

### 4. Enhanced Section Extraction
**Priority**: Medium
**Current limitation**: Simplified section extraction for `edit` and `replace_in_file` tools
**Need**: More sophisticated logic to handle complex formatting scenarios

### 5. Testing & Integration
**Tasks**:
- End-to-end testing with real format commands
- Integration tests with different tool syntaxes
- Performance testing with large files
- Error handling validation

## Key Files Modified

### Core Implementation
- `crates/code_assistant/src/types.rs` - Extended Project struct
- `crates/code_assistant/src/utils/file_updater.rs` - Core formatting logic
- `crates/code_assistant/src/tools/formatter.rs` - Tool syntax formatters
- `crates/code_assistant/src/format_on_save.rs` - Main handler (partially used)

### Tool System
- `crates/code_assistant/src/tools/core/tool.rs` - Modified trait signature
- `crates/code_assistant/src/tools/core/dyn_tool.rs` - Updated trait object handling
- `crates/code_assistant/src/agent/runner.rs` - Parameter change detection

### Configuration & Testing
- `crates/code_assistant/src/config.rs` - Updated Project initialization
- `crates/code_assistant/src/tests/mocks.rs` - Updated mock implementations
- Various tool implementation files - Updated to return tuples

## Design Principles

1. **Always format when configured**: User expectations must be met
2. **Graceful degradation**: Skip parameter updates if reconstruction fails, but still format
3. **LLM recovery**: LLM can recover from failed edits by re-reading files
4. **Backward compatibility**: Existing functionality must continue working
5. **Performance**: Minimize overhead for projects without format-on-save

## Next Steps

The most critical next step is resolving the async trait object compatibility issue. Once that's solved, the remaining integration work should be straightforward given the solid foundation already implemented in Phases 1 and 2.
