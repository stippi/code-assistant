use crate::types::ToolDefinition;
use serde_json::json;

/// Collection of all available tool definitions
#[derive(Debug, Clone)]
pub struct Tools;

impl Tools {
    /// Returns all available tool definitions
    pub fn all() -> Vec<ToolDefinition> {
        vec![
            Self::execute_command(),
            Self::search_files(),
            Self::list_files(),
            Self::read_files(),
            Self::summarize(),
            Self::replace_in_file(),
            Self::write_file(),
            Self::delete_files(),
            Self::ask_user(),
            Self::message_user(),
            Self::complete_task(),
        ]
    }

    pub fn mcp() -> Vec<ToolDefinition> {
        vec![
            Self::execute_command(),
            Self::search_files(),
            Self::list_files(),
            Self::read_files(),
            Self::summarize(),
            Self::replace_in_file(),
            Self::write_file(),
            Self::delete_files(),
        ]
    }

    pub fn execute_command() -> ToolDefinition {
        ToolDefinition {
            name: "execute_command".to_string(),
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

    pub fn search_files() -> ToolDefinition {
        ToolDefinition {
            name: "search_files".to_string(),
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

    pub fn list_files() -> ToolDefinition {
        ToolDefinition {
            name: "list_files".to_string(),
            description: "List files in a directory".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "description": "Directory paths relative to project root",
                        "items": {
                            "type": "string"
                        }
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "Maximum directory depth"
                    }
                },
                "required": ["paths"]
            }),
        }
    }

    pub fn read_files() -> ToolDefinition {
        ToolDefinition {
            name: "read_files".to_string(),
            description: "Load files into working memory".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "description": "Paths to the files relative to the workspace root directory",
                        "items": {
                            "type": "string"
                        }
                    }
                },
                "required": ["paths"]
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

    pub fn replace_in_file() -> ToolDefinition {
        ToolDefinition {
            name: "replace_in_file".to_string(),
            description: "Replace sections in a file using search/replace blocks".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to modify"
                    },
                    "replacements": {
                        "type": "array",
                        "description": "List of search/replace pairs",
                        "items": {
                            "type": "object",
                            "properties": {
                                "search": {
                                    "type": "string",
                                    "description": "Exact content to find. Make sure it is unique in the file."
                                },
                                "replace": {
                                    "type": "string",
                                    "description": "Content to replace with"
                                }
                            },
                            "required": ["search", "replace"]
                        }
                    }
                },
                "required": ["path", "replacements"]
            }),
        }
    }

    pub fn write_file() -> ToolDefinition {
        ToolDefinition {
            name: "write_file".to_string(),
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

    pub fn delete_files() -> ToolDefinition {
        ToolDefinition {
            name: "delete_files".to_string(),
            description: "Delete files from the workspace. This operation cannot be undone!"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "description": "Paths to the files relative to the workspace root directory",
                        "items": {
                            "type": "string"
                        }
                    }
                },
                "required": ["paths"]
            }),
        }
    }

    pub fn ask_user() -> ToolDefinition {
        ToolDefinition {
            name: "ask_user".to_string(),
            description:
                "Ask the user a question. Use for clarifications, feedback or confirmation."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question for the user"
                    }
                },
                "required": ["question"]
            }),
        }
    }

    pub fn message_user() -> ToolDefinition {
        ToolDefinition {
            name: "message_user".to_string(),
            description: "Complete the task".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "A final message for the user"
                    }
                },
                "required": ["message"]
            }),
        }
    }

    pub fn complete_task() -> ToolDefinition {
        ToolDefinition {
            name: "complete_task".to_string(),
            description: "Complete the task".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "A final message for the user"
                    }
                },
                "required": ["message"]
            }),
        }
    }
}
