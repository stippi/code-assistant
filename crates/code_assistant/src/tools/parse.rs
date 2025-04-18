use crate::types::{FileReplacement, Tool, ToolError};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::trace;
use web;

pub const TOOL_TAG_PREFIX: &str = "tool:";
const PARAM_TAG_PREFIX: &str = "param:";

/// Represents a parsed path with optional line ranges
#[derive(Debug, Clone)]
pub struct PathWithLineRange {
    pub path: PathBuf,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
}

impl PathWithLineRange {
    /// Parse a path string that may contain line ranges like "file.txt:10-20"
    pub fn parse(path_str: &str) -> Result<Self, ToolError> {
        // Check if the path contains a colon (not part of Windows drive letter)
        if let Some(colon_pos) = path_str.rfind(':') {
            // Skip Windows drive letter (e.g., C:)
            if colon_pos > 1
                || (colon_pos == 1 && !path_str.chars().next().unwrap().is_alphabetic())
            {
                let (file_path, line_range) = path_str.split_at(colon_pos);
                let line_range = &line_range[1..]; // Skip the colon

                // Parse the line range
                if line_range.is_empty() {
                    // Just a colon with nothing after it, treat as normal path
                    return Ok(Self {
                        path: PathBuf::from(path_str),
                        start_line: None,
                        end_line: None,
                    });
                }

                if let Some(dash_pos) = line_range.find('-') {
                    // Range with dash: file.txt:10-20
                    let (start, end) = line_range.split_at(dash_pos);
                    let end = &end[1..]; // Skip the dash

                    let start_line = if start.is_empty() {
                        None // file.txt:-20
                    } else {
                        Some(start.parse::<usize>().map_err(|_| {
                            ToolError::ParseError(format!("Invalid start line number: {}", start))
                        })?)
                    };

                    let end_line = if end.is_empty() {
                        None // file.txt:10-
                    } else {
                        Some(end.parse::<usize>().map_err(|_| {
                            ToolError::ParseError(format!("Invalid end line number: {}", end))
                        })?)
                    };

                    return Ok(Self {
                        path: PathBuf::from(file_path),
                        start_line,
                        end_line,
                    });
                } else {
                    // Single line: file.txt:15
                    let line_num = line_range.parse::<usize>().map_err(|_| {
                        ToolError::ParseError(format!("Invalid line number: {}", line_range))
                    })?;

                    return Ok(Self {
                        path: PathBuf::from(file_path),
                        start_line: Some(line_num),
                        end_line: Some(line_num),
                    });
                }
            }
        }

        // No line range specified
        Ok(Self {
            path: PathBuf::from(path_str),
            start_line: None,
            end_line: None,
        })
    }
}

// Helper function to parse JSON arrays containing paths with optional line ranges
fn parse_path_array(arr: &serde_json::Value, param_name: &str) -> Result<Vec<PathBuf>, ToolError> {
    arr.as_array()
        .ok_or_else(|| {
            ToolError::ParseError(format!("Missing required parameter: {} array", param_name))
        })?
        .iter()
        .map(|p| {
            let path_str = p.as_str().ok_or_else(|| {
                ToolError::ParseError(format!("Invalid path in {} array", param_name))
            })?;

            let parsed = PathWithLineRange::parse(path_str)?;
            Ok(parsed.path)
        })
        .collect::<Result<Vec<_>, _>>()
}

pub fn parse_tool_xml(xml: &str) -> Result<Tool, ToolError> {
    trace!("Parsing XML:\n{}", xml);

    let tool_name = xml
        .trim()
        .strip_prefix(&format!("<{}", TOOL_TAG_PREFIX))
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.strip_suffix('>'))
        .ok_or_else(|| ToolError::ParseError("Missing tool name".into()))?
        .to_string();

    trace!("Found tool name: {}", tool_name);

    let mut params: HashMap<String, Vec<String>> = HashMap::new();
    let mut current_param = String::new();
    let mut content_start = 0;

    let mut chars = xml.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '<' {
            // Check for parameter tag
            let rest = &xml[i..];
            trace!("Found '<', rest of string: {}", rest);
            if rest.starts_with(&format!("</{}", PARAM_TAG_PREFIX)) {
                // Closing tag
                let param_name = rest[format!("</{}", PARAM_TAG_PREFIX).len()..] // skip the "</param:"
                    .split('>')
                    .next()
                    .ok_or_else(|| ToolError::ParseError("Invalid closing tag format".into()))?;
                trace!("Found closing tag for: {}", param_name);
                if param_name == current_param {
                    let content = &xml[content_start..i];
                    trace!("Found content for {}: {}", current_param, content);
                    params
                        .entry(current_param.clone())
                        .or_default()
                        .push(content.to_string());
                    current_param.clear();
                }
            } else if let Some(param_start) = rest.strip_prefix(&format!("<{}", PARAM_TAG_PREFIX)) {
                // Opening tag
                if let Some(param_name) = param_start.split('>').next() {
                    current_param = param_name.to_string();
                    content_start = i + format!("<{}{}>", PARAM_TAG_PREFIX, param_name).len();
                    trace!("Found param start: {} at {}", current_param, content_start);
                }
            }
        }
    }

    trace!("Final parameters: {:?}", params);
    parse_tool_from_params(&tool_name, &params)
}

pub(crate) fn parse_search_replace_blocks(
    content: &str,
) -> Result<Vec<FileReplacement>, ToolError> {
    let mut replacements = Vec::new();
    let mut lines = content.lines().peekable();

    while let Some(line) = lines.next() {
        // Match the exact marker without trimming leading whitespace
        let is_search_all = line.trim_end() == "<<<<<<< SEARCH_ALL";
        let is_search = line.trim_end() == "<<<<<<< SEARCH";

        if is_search || is_search_all {
            let mut search = String::new();
            let mut replace = String::new();

            // Collect search content until we find the separator
            while let Some(line) = lines.next() {
                if line.trim_end() == "=======" {
                    break;
                }
                if !search.is_empty() {
                    search.push('\n');
                }
                search.push_str(line);
            }

            // Collect replace content
            let end_marker = if is_search_all {
                ">>>>>>> REPLACE_ALL"
            } else {
                ">>>>>>> REPLACE"
            };
            while let Some(current_line) = lines.next() {
                // Check for end marker
                if current_line.trim_end() == end_marker {
                    break;
                }

                // Check if the next line is the end marker and the current line is a separator
                // This handles the case when LLM accidentally adds a separator right before the end marker
                if current_line.trim_end() == "=======" {
                    if let Some(next_line) = lines.peek() {
                        if next_line.trim_end() == end_marker {
                            // Skip this separator - it's a mistake before the end marker
                            continue;
                        }
                    }
                }

                // Regular content line - add to replace content
                if !replace.is_empty() {
                    replace.push('\n');
                }
                replace.push_str(current_line);
            }

            replacements.push(FileReplacement {
                search,
                replace,
                replace_all: is_search_all,
            });
        }
    }

    Ok(replacements)
}

// Function to get the first required parameter or error
fn get_required_param<'a>(
    params: &'a HashMap<String, Vec<String>>,
    key: &str,
) -> Result<&'a String, ToolError> {
    params
        .get(key)
        .ok_or_else(|| ToolError::ParseError(format!("Missing required parameter: {}", key)))?
        .first()
        .ok_or_else(|| ToolError::ParseError(format!("Parameter {} is empty", key)))
}

// Function to get an optional parameter
fn get_optional_param<'a>(
    params: &'a HashMap<String, Vec<String>>,
    key: &str,
) -> Option<&'a String> {
    params.get(key).and_then(|v| v.first())
}

pub fn parse_tool_from_params(
    tool_name: &str,
    params: &HashMap<String, Vec<String>>,
) -> Result<Tool, ToolError> {
    match tool_name {
        "update_plan" => Ok(Tool::UpdatePlan {
            plan: get_required_param(params, "plan")?.clone(),
        }),

        "search_files" => Ok(Tool::SearchFiles {
            project: get_required_param(params, "project")?.clone(),
            regex: get_required_param(params, "regex")?.clone(),
        }),

        "list_files" => Ok(Tool::ListFiles {
            project: get_required_param(params, "project")?.clone(),
            paths: params
                .get("path")
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: path".into()))?
                .iter()
                .map(|s| PathBuf::from(s.trim()))
                .collect(),
            max_depth: get_optional_param(params, "max_depth")
                .map(|v| v.trim().parse::<usize>())
                .transpose()
                .map_err(|_| ToolError::ParseError("Invalid max_depth parameter".into()))?,
        }),

        "read_files" => Ok(Tool::ReadFiles {
            project: get_required_param(params, "project")?.clone(),
            paths: params
                .get("path")
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: path".into()))?
                .iter()
                .map(|s| {
                    // Keep the original path string including line ranges in the PathBuf
                    Ok(PathBuf::from(s.trim()))
                })
                .collect::<Result<Vec<PathBuf>, ToolError>>()?,
        }),

        "summarize" => Ok(Tool::Summarize {
            project: get_required_param(params, "project")?.clone(),
            path: PathBuf::from(get_required_param(params, "path")?),
            summary: get_required_param(params, "summary")?.clone(),
        }),

        "replace_in_file" => Ok(Tool::ReplaceInFile {
            project: get_required_param(params, "project")?.clone(),
            path: PathBuf::from(get_required_param(params, "path")?),
            replacements: parse_search_replace_blocks(get_required_param(params, "diff")?)?,
        }),

        "write_file" => Ok(Tool::WriteFile {
            project: get_required_param(params, "project")?.clone(),
            path: PathBuf::from(get_required_param(params, "path")?),
            content: get_required_param(params, "content")?.clone(),
            append: get_optional_param(params, "append").map_or(false, |s| s == "true"),
        }),

        "delete_files" => Ok(Tool::DeleteFiles {
            project: get_required_param(params, "project")?.clone(),
            paths: params
                .get("path")
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: path".into()))?
                .iter()
                .map(|s| PathBuf::from(s.trim()))
                .collect(),
        }),

        "complete_task" => Ok(Tool::CompleteTask {
            message: get_required_param(params, "message")?.clone(),
        }),

        "execute_command" => Ok(Tool::ExecuteCommand {
            command_line: get_required_param(params, "command_line")?.clone(),
            working_dir: get_optional_param(params, "working_dir").map(PathBuf::from),
            project: get_required_param(params, "project")?.clone(),
        }),

        "web_search" => Ok(Tool::WebSearch {
            query: get_required_param(params, "query")?.clone(),
            hits_page_number: get_required_param(params, "hits_page_number")?
                .trim()
                .parse::<u32>()
                .map_err(|e| ToolError::ParseError(format!("Invalid hits_page_number: {}", e)))?,
        }),

        "web_fetch" => Ok(Tool::WebFetch {
            url: get_required_param(params, "url")?.clone(),
            selectors: params
                .get("selector")
                .map(|selectors| selectors.iter().map(|s| s.to_string()).collect()),
        }),

        "list_projects" => Ok(Tool::ListProjects),

        "perplexity_ask" => {
            let messages_param = get_required_param(params, "messages")?;
            let messages_json: Result<Vec<web::PerplexityMessage>, _> =
                serde_json::from_str(messages_param);

            match messages_json {
                Ok(messages) => Ok(Tool::PerplexityAsk { messages }),
                Err(_) => Err(ToolError::ParseError(
                    "Invalid messages format for perplexity_ask".into(),
                )),
            }
        }

        _ => Err(ToolError::UnknownTool(tool_name.to_string())),
    }
}

pub fn parse_tool_json(name: &str, params: &serde_json::Value) -> Result<Tool, ToolError> {
    // Helper to extract required project field
    let get_project = |p: &serde_json::Value| -> Result<String, ToolError> {
        Ok(p["project"] // Wrap the whole expression in Ok()
            .as_str()
            .ok_or_else(|| ToolError::ParseError("Missing required parameter: project".into()))?
            .to_string())
    };

    match name {
        "list_projects" => Ok(Tool::ListProjects),
        "update_plan" => Ok(Tool::UpdatePlan {
            plan: params["plan"]
                .as_str()
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: plan".into()))?
                .to_string(),
        }),
        "execute_command" => Ok(Tool::ExecuteCommand {
            project: get_project(params)?,
            command_line: params["command_line"]
                .as_str()
                .ok_or_else(|| {
                    ToolError::ParseError("Missing required parameter: command_line".into())
                })?
                .to_string(),
            working_dir: params
                .get("working_dir")
                .and_then(|d| d.as_str())
                .map(PathBuf::from),
        }),
        "search_files" => Ok(Tool::SearchFiles {
            project: get_project(params)?,
            regex: params["regex"]
                .as_str()
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: regex".into()))?
                .to_string(),
        }),
        "list_files" => Ok(Tool::ListFiles {
            project: get_project(params)?,
            paths: parse_path_array(&params["paths"], "paths")?,
            max_depth: params["max_depth"].as_u64().map(|d| d as usize),
        }),
        "read_files" => Ok(Tool::ReadFiles {
            project: get_project(params)?,
            paths: params["paths"]
                .as_array()
                .ok_or_else(|| {
                    ToolError::ParseError("Missing required parameter: paths array".into())
                })?
                .iter()
                .map(|p| {
                    let path_str = p.as_str().ok_or_else(|| {
                        ToolError::ParseError("Invalid path in paths array".into())
                    })?;

                    // Just use the original path string in the PathBuf
                    // The line range parsing will happen in the executor
                    Ok(PathBuf::from(path_str))
                })
                .collect::<Result<Vec<PathBuf>, ToolError>>()?,
        }),
        "summarize" => Ok(Tool::Summarize {
            project: get_project(params)?,
            path: PathBuf::from(
                params["path"].as_str().ok_or_else(|| {
                    ToolError::ParseError("Missing required parameter: path".into())
                })?,
            ),
            summary: params["summary"]
                .as_str()
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: summary".into()))?
                .to_string(),
        }),
        "replace_in_file" => {
            Ok(Tool::ReplaceInFile {
                project: get_project(params)?,
                path: PathBuf::from(params["path"].as_str().ok_or_else(|| {
                    ToolError::ParseError("Missing required parameter: path".into())
                })?),
                replacements: parse_search_replace_blocks(params["diff"].as_str().ok_or_else(
                    || ToolError::ParseError("Missing required parameter: diff".into()),
                )?)?,
            })
        }
        "write_file" => Ok(Tool::WriteFile {
            project: get_project(params)?,
            path: PathBuf::from(
                params["path"].as_str().ok_or_else(|| {
                    ToolError::ParseError("Missing required parameter: path".into())
                })?,
            ),
            content: params["content"]
                .as_str()
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: content".into()))?
                .to_string(),
            append: params
                .get("append")
                .and_then(|b| b.as_bool())
                .unwrap_or(false),
        }),
        "delete_files" => Ok(Tool::DeleteFiles {
            project: get_project(params)?,
            paths: parse_path_array(&params["paths"], "paths")?,
        }),
        "complete_task" => Ok(Tool::CompleteTask {
            message: params["message"]
                .as_str()
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: message".into()))?
                .to_string(),
        }),
        "web_search" => Ok(Tool::WebSearch {
            query: params["query"]
                .as_str()
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: query".into()))?
                .to_string(),
            hits_page_number: params["hits_page_number"].as_u64().ok_or_else(|| {
                ToolError::ParseError("Missing required parameter: hits_page_number".into())
            })? as u32,
        }),
        "web_fetch" => Ok(Tool::WebFetch {
            url: params["url"]
                .as_str()
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: url".into()))?
                .to_string(),
            selectors: params["selectors"].as_array().map(|arr| {
                arr.iter()
                    .map(|v| v.as_str().unwrap_or_default().to_string())
                    .collect()
            }),
        }),
        "perplexity_ask" => {
            // Parse the messages array
            let messages = params["messages"]
                .as_array()
                .ok_or_else(|| {
                    ToolError::ParseError("Missing required parameter: messages array".into())
                })?
                .iter()
                .map(|msg| {
                    let role = msg["role"]
                        .as_str()
                        .ok_or_else(|| ToolError::ParseError("Missing 'role' in message".into()))?
                        .to_string();

                    let content = msg["content"]
                        .as_str()
                        .ok_or_else(|| {
                            ToolError::ParseError("Missing 'content' in message".into())
                        })?
                        .to_string();

                    Ok(web::PerplexityMessage { role, content })
                })
                .collect::<Result<Vec<web::PerplexityMessage>, ToolError>>()?;

            Ok(Tool::PerplexityAsk { messages })
        }
        _ => Err(ToolError::UnknownTool(name.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse::parse_search_replace_blocks;

    #[test]
    fn test_parse_search_replace_blocks_normal() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "if a > b {\n",
            "    return a;\n",
            "}\n",
            "=======\n",
            "if a >= b {\n",
            "    return a;\n",
            "}\n",
            ">>>>>>> REPLACE"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].search, "if a > b {\n    return a;\n}");
        assert_eq!(result[0].replace, "if a >= b {\n    return a;\n}");
        assert_eq!(result[0].replace_all, false);
    }

    #[test]
    fn test_parse_search_replace_blocks_multiple() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "if a > b {\n",
            "=======\n",
            "if a >= b {\n",
            ">>>>>>> REPLACE\n",
            "<<<<<<< SEARCH\n",
            "return a;\n",
            "=======\n",
            "return a + 1;\n",
            ">>>>>>> REPLACE"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].search, "if a > b {");
        assert_eq!(result[0].replace, "if a >= b {");
        assert_eq!(result[1].search, "return a;");
        assert_eq!(result[1].replace, "return a + 1;");
    }

    #[test]
    fn test_parse_search_replace_blocks_with_second_separator() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "if a > b {\n",
            "    return a;\n",
            "}\n",
            "=======\n",
            "if a >= b {\n",
            "    // Add a comment\n",
            "=======\n",
            ">>>>>>> REPLACE"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].search, "if a > b {\n    return a;\n}");
        assert_eq!(result[0].replace, "if a >= b {\n    // Add a comment");
    }

    #[test]
    fn test_parse_search_replace_blocks_empty_sections() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "// This comment will be removed\n",
            "=======\n",
            ">>>>>>> REPLACE"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].search, "// This comment will be removed");
        assert_eq!(result[0].replace, "");
    }

    #[test]
    fn test_parse_search_replace_all_blocks() {
        let content = concat!(
            "<<<<<<< SEARCH_ALL\n",
            "console.log(\n",
            "=======\n",
            "logger.debug(\n",
            ">>>>>>> REPLACE_ALL"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].search, "console.log(");
        assert_eq!(result[0].replace, "logger.debug(");
        assert_eq!(result[0].replace_all, true);
    }

    #[test]
    fn test_parse_mixed_search_replace_blocks() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "function test() {\n",
            "=======\n",
            "function renamed() {\n",
            ">>>>>>> REPLACE\n",
            "<<<<<<< SEARCH_ALL\n",
            "console.log(\n",
            "=======\n",
            "logger.debug(\n",
            ">>>>>>> REPLACE_ALL"
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].search, "function test() {");
        assert_eq!(result[0].replace, "function renamed() {");
        assert_eq!(result[0].replace_all, false);
        assert_eq!(result[1].search, "console.log(");
        assert_eq!(result[1].replace, "logger.debug(");
        assert_eq!(result[1].replace_all, true);
    }
}
