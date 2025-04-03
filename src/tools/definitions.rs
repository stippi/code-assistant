use crate::types::{ToolDefinition, Tools};
use serde_json::json;

impl Tools {
    /// Returns all available tool definitions
    pub fn all() -> Vec<ToolDefinition> {
        vec![
            Self::update_plan(),
            Self::execute_command(),
            Self::search_files(),
            Self::list_files(),
            Self::read_files(),
            Self::summarize(),
            Self::replace_in_file(),
            Self::write_file(),
            Self::delete_files(),
            Self::web_search(),
            Self::web_fetch(),
        ]
    }

    pub fn mcp() -> Vec<ToolDefinition> {
        vec![
            Self::list_projects(),
            Self::execute_command(),
            Self::search_files(),
            Self::list_files(),
            Self::read_files(),
            Self::replace_in_file(),
            Self::write_file(),
            Self::delete_files(),
            Self::web_search(),
            Self::web_fetch(),
        ]
    }

    pub fn list_projects() -> ToolDefinition {
        ToolDefinition {
            name: "list_projects".to_string(),
            description: "List all available projects".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    pub fn update_plan() -> ToolDefinition {
        ToolDefinition {
            name: "update_plan".to_string(),
            description: "Create or replace your plan for accomplishing the task".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "string",
                        "description": "Your plan in markdown format, i.e. a structured list."
                    }
                },
                "required": ["plan"]
            }),
        }
    }

    pub fn execute_command() -> ToolDefinition {
        ToolDefinition {
            name: "execute_command".to_string(),
            description: "Execute a command line program within a specified project".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project context for the command"
                    },
                    "command_line": {
                        "type": "string",
                        "description": "The complete command to execute"
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Optional: working directory for the command (relative to project root)"
                    }
                },
                "required": ["project", "command_line"]
            }),
        }
    }

    pub fn search_files() -> ToolDefinition {
        ToolDefinition {
            name: "search_files".to_string(),
            description: "Search for text in files within a specified project using regex in Rust syntax. This tool searches for specific content across multiple files, displaying each match with context.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project to search within"
                    },
                    "regex": {
                        "type": "string",
                        "description": "The regex pattern to search for. Supports Rust regex syntax including character classes, quantifiers, etc."
                    }
                },
                "required": ["project", "regex"]
            }),
        }
    }

    pub fn list_files() -> ToolDefinition {
        ToolDefinition {
            name: "list_files".to_string(),
            description: "List files in directories within a specified project".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project context"
                    },
                    "paths": {
                        "type": "array",
                        "description": "Directory paths relative to project root",
                        "items": {
                            "type": "string"
                        }
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "Optional: Maximum directory depth"
                    }
                },
                "required": ["project", "paths"]
            }),
        }
    }

    pub fn read_files() -> ToolDefinition {
        let description = concat!(
            "Load files into working memory. You can specify line ranges by appending them to the file path using a colon.\n\n",
            "Examples:\n",
            "- file.txt - Read the entire file. Prefer this form unless you are absolutely sure you need only a section of the file.\n",
            "- file.txt:10-20 - Read only lines 10 to 20\n",
            "- file.txt:10- - Read from line 10 to the end\n",
            "- file.txt:-20 - Read from the beginning to line 20\n",
            "- file.txt:15 - Read only line 15");

        ToolDefinition {
            name: "read_files".to_string(),
            description: description.to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project containing the files"
                    },
                    "paths": {
                        "type": "array",
                        "description": "Paths to the files relative to the project root directory. Can include line ranges using 'file.txt:10-20' syntax.",
                        "items": {
                            "type": "string"
                        }
                    }
                },
                "required": ["project", "paths"]
            }),
        }
    }

    pub fn summarize() -> ToolDefinition {
        ToolDefinition {
            name: "summarize".to_string(),
            description:
                "Replace resource content with a summary in working memory, unloading the full content. The purpose of this tool is to free up precious space in the working memory by keeping only relevant information from a resource."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "resources": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "Path to the resource to summarize"
                                },
                                "summary": {
                                    "type": "string",
                                    "description": "Your summary of the resource contents"
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
            description: "Replace sections in a file within a specified project using search/replace blocks. By default, each search text must match exactly once in the file, but you can use SEARCH_ALL/REPLACE_ALL blocks to replace all occurrences of a pattern.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project containing the file"
                    },
                    "path": {
                        "type": "string",
                        "description": "Path to the file to modify (relative to project root)"
                    },
                    "diff": {
                        "type": "string",
                        "description": "One or more SEARCH/REPLACE or SEARCH_ALL/REPLACE_ALL blocks following either of these formats:\n<<<<<<< SEARCH\n[exact content to find]\n=======\n[new content to replace with]\n>>>>>>> REPLACE\n\nOR\n\n<<<<<<< SEARCH_ALL\n[content pattern to find]\n=======\n[new content to replace with]\n>>>>>>> REPLACE_ALL\n\nWith SEARCH/REPLACE blocks, the search content must match exactly one location. With SEARCH_ALL/REPLACE_ALL blocks, all occurrences of the pattern will be replaced."
                    }
                },
                "required": ["project", "path", "diff"]
            }),
        }
    }

    pub fn write_file() -> ToolDefinition {
        ToolDefinition {
            name: "write_file".to_string(),
            description:
                "Creates or overwrites a file. Use for new files or when updating most content of a file. \
                         For smaller updates, prefer to use replace_in_file. ALWAYS provide the contents \
                         of the COMPLETE file, especially when overwriting existing files!! \
                         If the file to write is large, write it in chunks making use of the 'append' parameter. \
                         Always end your turn after using this tool, especially when using 'append'. \
                         This avoids hitting an output token limit when replying."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project context"
                    },
                    "path": {
                        "type": "string",
                        "description": "Path to create or overwrite (relative to project root)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write (make sure it's the complete file)"
                    },
                    "append": {
                        "type": "boolean",
                        "description": "Optional: Whether to append to the file. Default is false."
                    }
                },
                "required": ["project", "path", "content"]
            }),
        }
    }

    pub fn delete_files() -> ToolDefinition {
        ToolDefinition {
            name: "delete_files".to_string(),
            description: "Delete files from a specified project. This operation cannot be undone!"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Name of the project containing the files"
                    },
                    "paths": {
                        "type": "array",
                        "description": "Paths to the files relative to the project root directory",
                        "items": {
                            "type": "string"
                        }
                    }
                },
                "required": ["project", "paths"]
            }),
        }
    }

    pub fn web_search() -> ToolDefinition {
        ToolDefinition {
            name: "web_search".to_string(),
            description: "Search the web using DuckDuckGo".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query"
                    },
                    "hits_page_number": {
                        "type": "integer",
                        "description": "Page number for pagination (1-based)",
                        "minimum": 1
                    }
                },
                "required": ["query", "hits_page_number"]
            }),
        }
    }

    pub fn web_fetch() -> ToolDefinition {
        ToolDefinition {
            name: "web_fetch".to_string(),
            description: "Fetch and extract content from a web page".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL of the web page to fetch"
                    },
                    "selectors": {
                        "type": "array",
                        "description": "Optional CSS selectors to extract specific content",
                        "items": {
                            "type": "string"
                        }
                    }
                },
                "required": ["url"]
            }),
        }
    }
}
