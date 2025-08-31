# Format-on-Save Feature Implementation

This document describes the implementation of the format-on-save feature that automatically formats files after they are modified by the code assistant and keeps the LLM's mental model synchronized with the formatted code by updating tool inputs when safe to do so.

## Overview & The Core Problem

When an LLM generates code and the editor or project tooling auto-formats the file, the LLM's internal picture of the file (based on the tool inputs it just produced) can become stale. Subsequent edits may then fail because the assistant is searching for unformatted text that no longer exists.

Solution, at a glance:
- Run the appropriate formatter after file modifications (based on project configuration)
- Attempt to reconstruct the formatted replacement(s) and update tool inputs so it appears the LLM produced formatted text from the start
- Fall back gracefully when not confident in replacement reconstruction (still format, but do not rewrite tool parameters)

## Configuration

Project-level configuration supports format-on-save with simple glob-to-command mappings:

```json
{
  "my-rust-project": {
    "path": "/path/to/my/rust/project",
    "format_on_save": {
      "**/*.rs": "cargo fmt"
    }
  },
  "my-js-project": {
    "path": "/path/to/my/js/project",
    "format_on_save": {
      "*.js": "prettier --write {path}",
      "*.ts": "prettier --write {path}",
      "*.json": "prettier --write {path}"
    }
  }
}
```

Key points:
- Optional {path} placeholder: If present, it is replaced with the relative path (quoted appropriately); if absent, the command is executed as-is in the project root
  - Example: cargo fmt (no file argument), prettier/taplo/rustfmt usually take a file argument and should use {path}
- Deterministic matching: patterns are matched in sorted order for predictability

Convenience methods on Project:
- formatter_template_for(path) -> Option<String>
- format_command_for(path) -> Option<String>, which applies {path} via utils::build_format_command

## Implementation Status

### ✅ Core text replacement + normalization
- File: crates/code_assistant/src/utils/file_updater.rs
- Features:
  - Split responsibilities: find_replacement_matches (detects overlapping/adjacent), apply_matches (applies matches), and higher-level apply_replacements_normalized
  - Normalization of content (line endings, trailing whitespace) to make matching robust

### ✅ Stable range extraction and conservative reconstruction
- File: crates/code_assistant/src/utils/file_updater.rs
- StableRange extraction now retains whitespace-only anchors (no trimming) so whitespace doesn’t bleed into replacements
- reconstruct_formatted_replacements now uses conservative guards:
  - Locates surrounding stable ranges in the formatted content (falls back to file edges for start/end-of-file matches)
  - Only updates the replacement text when the formatted slice is equivalent to the original replacement modulo whitespace
  - Skips updates when anchors cannot be confidently resolved or when matches are adjacent/overlapping

### ✅ Tool formatter system
- File: crates/code_assistant/src/tools/formatter.rs
- Provides formatters for the different tool syntaxes (Native, XML, Caret)

### ✅ Tool trait + message history sync
- Files: crates/code_assistant/src/tools/core/tool.rs, dyn_tool.rs, and agent/runner.rs
- Tool::execute takes a mutable Self::Input; when a tool updates its input during execution, the agent updates the message history accordingly

### ✅ Project-centric formatter selection and templating
- File: crates/code_assistant/src/types.rs (Project methods)
- Project::formatter_template_for and Project::format_command_for centralize glob matching and {path} templating
- Tools call project.format_command_for(&rel_path)

### ✅ Tool integration
- Edit (crates/code_assistant/src/tools/impls/edit.rs)
  - After applying the edit, runs formatter (if configured)
  - Attempts reconstruction; on success, updates input.old_text/new_text (typically new_text only)
  - Updates working memory with the final on-disk content
- ReplaceInFile (crates/code_assistant/src/tools/impls/replace_in_file.rs)
  - Integrated format-on-save; calls Explorer’s format-aware apply
  - If updated replacements are returned, regenerates the diff string to reflect formatted REPLACE text
  - Supports multiple SEARCH/REPLACE blocks (updates only those it can confidently reconstruct)
- WriteFile (crates/code_assistant/src/tools/impls/write_file.rs)
  - After writing, runs formatter (if configured), re-reads the file, and overwrites input.content with the formatted content so follow-up edits align with reality

### ✅ Explorer integration
- File: crates/code_assistant/src/explorer.rs (real explorer)
- Mock: crates/code_assistant/src/tests/mocks.rs
  - MockExplorer simulates formatting by replacing file contents after a format command
  - On command failure (success == false), returns None for updated replacements (graceful failure)

## Technical Architecture

Format-aware workflow:
1. Tools compute file modifications (edit, replace blocks, write)
2. If project.format_command_for(rel_path) returns Some(cmd), run the formatter in project root
3. When safe: reconstruct formatted replacement text via stable ranges and conservative checks
4. Update tool inputs (and for replace_in_file, re-render the diff) to match formatted content
5. Working memory is updated with the final file content

Conflict handling and failure modes:
- Overlapping matches: error (probably not what the LLM intended)
- Adjacent matches: skip parameter reconstruction (but still apply formatting)
- Formatting command failure: keep modified content, do not update parameters
- Reconstruction failures: apply formatting, skip parameter updates

## Tests

Representative tests:
- Edit with realistic scenario: file starts formatted; replacement text is unformatted; format-on-save normalizes it; input.new_text updated; working memory updated
- ReplaceInFile with multiple SEARCH/REPLACE blocks: unformatted replacements; formatter normalizes; diff updated for the replacements we’re confident about
- Glob and command templating tests: prettier with {path}, cargo fmt without file arguments

## Key Files
- Project and configuration: crates/code_assistant/src/types.rs, crates/code_assistant/src/config.rs
- File updater and reconstruction: crates/code_assistant/src/utils/file_updater.rs
- Command templating and quoting: crates/code_assistant/src/utils/command.rs (build_format_command)
- Tools: crates/code_assistant/src/tools/impls/{edit.rs, replace_in_file.rs, write_file.rs}
- Explorer and mocks: crates/code_assistant/src/explorer.rs, crates/code_assistant/src/tests/mocks.rs
- Tool formatting and history updates: crates/code_assistant/src/tools/formatter.rs, crates/code_assistant/src/agent/runner.rs

## Design Principles
- Always format when configured
- Conservative parameter updates: only when confident
- Graceful degradation: formatting never blocks the write/edit; parameter updates can be skipped
- Deterministic behavior: glob matching order is stable

## Remaining Work / Next Steps
- Replace-all reconstruction: explore safe heuristics for updating REPLACE_ALL blocks
- Specificity/precedence for overlapping formatter patterns: consider most-specific match (e.g., fewer wildcards or longest match)
- Unit tests for Project::formatter_template_for and format_command_for
- Broader integration tests across languages/formatters

## Notes
- Security: formatter commands are user-configured and executed on the user’s machine. Broader sandboxing and access control are out of scope for this feature but may be addressed at the agent level.
- Concurrency/races: handling concurrent external edits/formatters is a known risk; outside current scope.
