# Format-on-Save Feature

This document describes the format-on-save feature that automatically formats files after they are modified by the code assistant, while maintaining consistency with the LLM's mental model of the code.

## Overview

The format-on-save feature addresses a common problem: when an LLM generates code that gets auto-formatted after saving, the LLM's mental model of the file becomes inconsistent with the actual file contents. This can cause subsequent edits to fail because the LLM expects the code to be in its original (unformatted) state.

The solution is to:
1. Run format commands after file modifications
2. Update the tool parameters in the message history to match the formatted output
3. Make it appear to the LLM that it generated perfectly formatted code from the beginning

## Configuration

Add a `format_on_save` field to your project configuration in `~/.config/code-assistant/projects.json`:

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
  },
  "basic-project": {
    "path": "/path/to/basic/project"
    // No format_on_save field means no formatting
  }
}
```

The `format_on_save` field is a map where:
- **Keys** are file patterns (using glob syntax like `*.rs`, `*.js`, etc.)
- **Values** are shell commands to run for formatting files matching the pattern

## How It Works

### 1. File Modification Detection

When the code assistant executes file modification tools (`write_file`, `edit`, `replace_in_file`), the format-on-save handler checks if the modified file matches any patterns in the project's configuration.

### 2. Format Command Execution

If a matching pattern is found, the corresponding format command is executed in the project directory. For example:
- `cargo fmt` for Rust files
- `prettier --write` for JavaScript/TypeScript files
- `black` for Python files

### 3. Content Comparison

The handler compares the original content with the formatted content. If they differ, it proceeds to update the tool parameters.

### 4. Tool Parameter Synchronization

The original tool parameters are updated to reflect the formatted content:
- For `write_file`: The `content` parameter is updated with the formatted file contents
- For `edit`: The `new_text` parameter is updated with the formatted replacement text
- For `replace_in_file`: The replacement blocks are updated (future implementation)

### 5. Message History Update

The message history is updated so that the LLM's tool calls appear to have generated the formatted code originally. This maintains consistency between the LLM's mental model and the actual file state.

## Implementation Details

### Supported Tools

Currently, the format-on-save feature supports:
- ✅ `write_file` - Full file content formatting
- ✅ `edit` - Partial implementation (simplified)
- ⚠️ `replace_in_file` - Placeholder (not yet implemented)

### Tool Syntax Support

The feature works with all three tool syntaxes:
- **Native**: JSON-based function calls
- **XML**: `<tool:name>` syntax
- **Caret**: `^^^tool_name` syntax

### Limitations

1. **Edit Tool Complexity**: Extracting the exact formatted section for `edit` operations is complex and currently uses a simplified approach.

2. **Replace Tool**: The `replace_in_file` tool is not yet fully implemented for format-on-save.

3. **Command Execution Context**: Format commands are executed in the project root directory. Make sure your format commands work from that location.

4. **Error Handling**: If a format command fails, the original content is preserved and a warning is logged.

## Example Workflow

1. User asks: "Create a new Rust function in `src/main.rs`"
2. LLM generates: `write_file` with unformatted Rust code
3. Code assistant writes the file
4. Format-on-save detects `*.rs` pattern matches `cargo fmt`
5. `cargo fmt` is executed, formatting the code
6. The `content` parameter in the tool call is updated with formatted code
7. Message history is updated to show the LLM generated formatted code
8. Future edits work correctly because LLM's mental model matches reality

## Benefits

- **Consistency**: LLM's mental model stays in sync with actual file contents
- **Code Quality**: Automatic formatting ensures consistent code style
- **Seamless Experience**: No manual intervention required
- **Flexible Configuration**: Per-project, per-file-type formatting rules

## Future Enhancements

- More sophisticated section extraction for `edit` operations
- Full `replace_in_file` support
- Support for format commands that modify multiple files
- Integration with language servers for more precise formatting
- Rollback mechanism if formatting introduces syntax errors
