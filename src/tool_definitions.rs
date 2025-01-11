use crate::types::ToolDefinition;
use serde_json::json;

/// Collection of all available tool definitions
#[derive(Debug, Clone)]
pub struct Tools;

impl Tools {
    /// Returns all available tool definitions
    pub fn all() -> Vec<ToolDefinition> {
        vec![
            Self::search(),
            Self::execute_command(),
            Self::list_files(),
            Self::load_file(),
            Self::summarize(),
            Self::update_file(),
            Self::write_file(),
            Self::delete_file(),
        ]
    }

    pub fn search() -> ToolDefinition {
        ToolDefinition {
            name: "search".to_string(),
            description: "Search for text in files with advanced options".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The text to search for. Supports regular expressions."
                    },
                    "path": {
                        "type": "string",
                        "description": "Optional: directory path to search in (relative to project root)"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Optional: maximum number of results to return"
                    },
                    "case_sensitive": {
                        "type": "boolean",
                        "description": "Optional: whether the search should be case-sensitive (default: false)"
                    },
                    "whole_words": {
                        "type": "boolean",
                        "description": "Optional: match whole words only (default: false)"
                    },
                    "mode": {
                        "type": "string",
                        "description": "Optional: search mode - 'exact' (default) for standard text search, or 'regex' for regular expressions",
                        "enum": ["exact", "regex"]
                    }
                },
                "required": ["query"]
            }),
        }
    }

    pub fn execute_command() -> ToolDefinition {
        ToolDefinition {
            name: "execute-command".to_string(),
            description: "Execute a command line program".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command_line": {
                        "type": "string",
                        "description": "The complete command to execute"
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Optional: working directory for the command"
                    }
                },
                "required": ["command_line"]
            }),
        }
    }

    pub fn list_files() -> ToolDefinition {
        ToolDefinition {
            name: "list-files".to_string(),
            description: "List files in a directory".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path relative to project root"
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "Maximum directory depth"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    pub fn load_file() -> ToolDefinition {
        ToolDefinition {
            name: "load-file".to_string(),
            description: "Load a file into working memory for access as a resource".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the file from project root"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    pub fn summarize() -> ToolDefinition {
        ToolDefinition {
            name: "summarize".to_string(),
            description:
                "Replace file content with a summary in working memory, unloading the full content."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "Path to the file to summarize"
                                },
                                "summary": {
                                    "type": "string",
                                    "description": "Your summary of the file contents"
                                }
                            },
                            "required": ["path", "summary"]
                        }
                    }
                },
                "required": ["files"]
            }),
        }
    }

    pub fn update_file() -> ToolDefinition {
        ToolDefinition {
            name: "update-file".to_string(),
            description: "Update sections in an existing file based on line numbers. IMPORTANT: Line numbers are 1-based, \
                         matching the line numbers shown when viewing file resources. The end_line is exclusive, \
                         meaning the section to replace ends before that line. For example, to replace lines 1-3, \
                         use start_line: 1, end_line: 4. To insert new content without replacing anything, \
                         use the same start_line and end_line. Provide the new content parameter first, \
                         then start_line and end_line parameter according to what needs to be replaced.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the file to update"
                    },
                    "updates": {
                        "type": "array",
                        "description": "List of updates to apply to the file",
                        "items": {
                            "type": "object",
                            "properties": {
                                "new_content": {
                                    "type": "string",
                                    "description": "The new content to insert (without line numbers)"
                                },
                                "start_line": {
                                    "type": "integer",
                                    "description": "First line number to replace (1-based, matching the displayed line numbers)"
                                },
                                "end_line": {
                                    "type": "integer",
                                    "description": "Line number right after the section to replace (1-based, matching the displayed line numbers)"
                                }
                            },
                            "required": ["new_content", "start_line", "end_line"]
                        }
                    }
                },
                "required": ["path", "updates"]
            }),
        }
    }

    pub fn write_file() -> ToolDefinition {
        ToolDefinition {
            name: "write-file".to_string(),
            description:
                "Creates or overwrites a file. Use for new files or when updating most of a file. \
                         For smaller updates prefer to use update-file."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to create or overwrite"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    pub fn delete_file() -> ToolDefinition {
        ToolDefinition {
            name: "delete-file".to_string(),
            description: "Delete a file from the workspace. This operation cannot be undone!"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the file to delete"
                    }
                },
                "required": ["path"]
            }),
        }
    }
}
