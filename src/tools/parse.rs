use crate::types::{FileReplacement, Tool, ToolError};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::trace;

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

fn parse_search_replace_blocks(content: &str) -> Result<Vec<FileReplacement>, ToolError> {
    let mut replacements = Vec::new();
    let mut lines = content.lines().peekable();

    while let Some(line) = lines.next() {
        if line.trim() == "<<<<<<< SEARCH" {
            let mut search = String::new();
            let mut replace = String::new();

            // Collect search content
            while let Some(line) = lines.next() {
                if line.trim() == "=======" {
                    break;
                }
                if !search.is_empty() {
                    search.push('\n');
                }
                search.push_str(line);
            }

            // Collect replace content
            while let Some(line) = lines.next() {
                if line.trim() == ">>>>>>> REPLACE" {
                    break;
                }
                if !replace.is_empty() {
                    replace.push('\n');
                }
                replace.push_str(line);
            }

            replacements.push(FileReplacement { search, replace });
        }
    }

    Ok(replacements)
}

pub fn parse_tool_from_params(
    tool_name: &str,
    params: &HashMap<String, Vec<String>>,
) -> Result<Tool, ToolError> {
    match tool_name {
        "update_plan" => Ok(Tool::UpdatePlan {
            plan: params
                .get("plan")
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: plan".into()))?
                .first()
                .ok_or_else(|| ToolError::ParseError("Plan parameter is empty".into()))?
                .to_string(),
        }),

        "search_files" => Ok(Tool::SearchFiles {
            regex: params
                .get("regex")
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: regex".into()))?
                .first()
                .ok_or_else(|| ToolError::ParseError("Regex parameter is empty".into()))?
                .to_string(),
        }),

        "list_files" => Ok(Tool::ListFiles {
            paths: params
                .get("path")
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: path".into()))?
                .iter()
                .map(|s| PathBuf::from(s.trim()))
                .collect(),
            max_depth: params
                .get("max_depth")
                .and_then(|v| v.first())
                .map(|v| v.trim().parse::<usize>())
                .transpose()
                .map_err(|_| ToolError::ParseError("Invalid max_depth parameter".into()))?,
        }),

        "read_files" => Ok(Tool::ReadFiles {
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
            resources: params
                .get("resource")
                .ok_or_else(|| {
                    ToolError::ParseError("Missing required parameter: resource".into())
                })?
                .iter()
                .filter_map(|line| {
                    let mut parts = line.splitn(2, ':');
                    Some((
                        PathBuf::from(parts.next()?.trim()),
                        parts.next()?.trim().to_string(),
                    ))
                })
                .collect(),
        }),

        "replace_in_file" => {
            Ok(Tool::ReplaceInFile {
                path: PathBuf::from(params.get("path").and_then(|v| v.first()).ok_or_else(
                    || ToolError::ParseError("Missing required parameter: path".into()),
                )?),
                replacements: parse_search_replace_blocks(
                    params.get("diff").and_then(|v| v.first()).ok_or_else(|| {
                        ToolError::ParseError("Missing required parameter: diff".into())
                    })?,
                )?,
            })
        }

        "write_file" => {
            let append = params
                .get("append")
                .map_or(false, |v| v.first().map_or(false, |s| s == "true"));

            Ok(Tool::WriteFile {
                path: PathBuf::from(params.get("path").and_then(|v| v.first()).ok_or_else(
                    || ToolError::ParseError("Missing required parameter: path".into()),
                )?),
                content: params
                    .get("content")
                    .and_then(|v| v.first())
                    .ok_or_else(|| {
                        ToolError::ParseError("Missing required parameter: content".into())
                    })?
                    .to_string(),
                append,
            })
        }

        "delete_files" => Ok(Tool::DeleteFiles {
            paths: params
                .get("path")
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: path".into()))?
                .iter()
                .map(|s| PathBuf::from(s.trim()))
                .collect(),
        }),

        "complete_task" => Ok(Tool::CompleteTask {
            message: params
                .get("message")
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: message".into()))?
                .first()
                .ok_or_else(|| ToolError::ParseError("Message parameter is empty".into()))?
                .to_string(),
        }),

        "execute_command" => Ok(Tool::ExecuteCommand {
            command_line: params
                .get("command_line")
                .ok_or_else(|| {
                    ToolError::ParseError("Missing required parameter: command_line".into())
                })?
                .first()
                .ok_or_else(|| ToolError::ParseError("Command line parameter is empty".into()))?
                .to_string(),
            working_dir: params
                .get("working_dir")
                .and_then(|v| v.first())
                .map(|v| PathBuf::from(v)),
        }),

        "web_search" => Ok(Tool::WebSearch {
            query: params
                .get("query")
                .and_then(|v| v.first())
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: query".into()))?
                .to_string(),
            hits_page_number: params
                .get("hits_page_number")
                .and_then(|v| v.first())
                .map(|v| v.trim().parse::<u32>())
                .transpose()
                .map_err(|e| ToolError::ParseError(format!("Invalid parameter value: {}", e)))?
                .ok_or_else(|| {
                    ToolError::ParseError("Missing required parameter: hits_page_number".into())
                })?,
        }),

        "web_fetch" => Ok(Tool::WebFetch {
            url: params
                .get("url")
                .and_then(|v| v.first())
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: url".into()))?
                .to_string(),
            selectors: params
                .get("selector")
                .map(|selectors| selectors.iter().map(|s| s.to_string()).collect()),
        }),

        _ => Err(ToolError::UnknownTool(tool_name.to_string())),
    }
}

pub fn parse_tool_json(name: &str, params: &serde_json::Value) -> Result<Tool, ToolError> {
    match name {
        "list_projects" => Ok(Tool::ListProjects),
        "open_project" => Ok(Tool::OpenProject {
            name: params["name"]
                .as_str()
                .ok_or_else(|| {
                    ToolError::ParseError("Missing required parameter: project name".into())
                })?
                .to_string(),
        }),
        "update_plan" => Ok(Tool::UpdatePlan {
            plan: params["plan"]
                .as_str()
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: plan".into()))?
                .to_string(),
        }),
        "execute_command" => Ok(Tool::ExecuteCommand {
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
            regex: params["regex"]
                .as_str()
                .ok_or_else(|| ToolError::ParseError("Missing required parameter: regex".into()))?
                .to_string(),
        }),
        "list_files" => Ok(Tool::ListFiles {
            paths: parse_path_array(&params["paths"], "paths")?,
            max_depth: params["max_depth"].as_u64().map(|d| d as usize),
        }),
        "read_files" => Ok(Tool::ReadFiles {
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
            resources: params["resources"]
                .as_array()
                .ok_or_else(|| {
                    ToolError::ParseError("Missing required parameter: resources array".into())
                })?
                .iter()
                .map(|f| -> Result<_, ToolError> {
                    Ok((
                        PathBuf::from(f["path"].as_str().ok_or_else(|| {
                            ToolError::ParseError("Missing path in resource entry".into())
                        })?),
                        f["summary"]
                            .as_str()
                            .ok_or_else(|| {
                                ToolError::ParseError("Missing summary in resource entry".into())
                            })?
                            .to_string(),
                    ))
                })
                .collect::<Result<Vec<_>, ToolError>>()?,
        }),
        "replace_in_file" => {
            Ok(Tool::ReplaceInFile {
                path: PathBuf::from(params["path"].as_str().ok_or_else(|| {
                    ToolError::ParseError("Missing required parameter: path".into())
                })?),
                replacements: parse_search_replace_blocks(params["diff"].as_str().ok_or_else(
                    || ToolError::ParseError("Missing required parameter: diff".into()),
                )?)?,
            })
        }
        "write_file" => Ok(Tool::WriteFile {
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
        _ => Err(ToolError::UnknownTool(name.to_string())),
    }
}
