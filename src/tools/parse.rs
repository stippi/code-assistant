use crate::types::FileUpdate;
use crate::types::Tool;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::debug;

pub const TOOL_TAG_PREFIX: &str = "tool:";
const PARAM_TAG_PREFIX: &str = "param:";

pub fn parse_tool_xml(xml: &str) -> Result<Tool> {
    debug!("Parsing XML:\n{}", xml);

    let tool_name = xml
        .trim()
        .strip_prefix(&format!("<{}", TOOL_TAG_PREFIX))
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.strip_suffix('>'))
        .ok_or_else(|| anyhow::anyhow!("Missing tool name"))?
        .to_string();

    debug!("Found tool name: {}", tool_name);

    let mut params = HashMap::new();
    let mut current_param = String::new();
    let mut current_value = String::new();

    for line in xml.lines() {
        let line = line.trim();
        debug!("Processing line: '{}'", line);

        if line.is_empty()
            || line == format!("<{}{}>", TOOL_TAG_PREFIX, tool_name)
            || line == format!("</{}{}>", TOOL_TAG_PREFIX, tool_name)
        {
            debug!("Skipping tool tag line");
            continue;
        }

        // Check for parameter start with prefix
        if let Some(param_start) = line.strip_prefix(&format!("<{}", PARAM_TAG_PREFIX)) {
            if !param_start.starts_with('/') {
                // Ignore closing tags
                if let Some(param_name) = param_start.split('>').next() {
                    current_param = param_name.to_string();
                    debug!("Found parameter start: {}", current_param);

                    // Check if it's a single-line parameter
                    if line.contains(&format!("</{}{}>", PARAM_TAG_PREFIX, current_param)) {
                        // Find positions for start/end tags
                        let content_start = line
                            .find('>')
                            .map(|pos| pos + 1)
                            .ok_or_else(|| anyhow::anyhow!("Invalid parameter tag format"))?;
                        let content_end = line
                            .find(&format!("</{}{}>", PARAM_TAG_PREFIX, current_param))
                            .ok_or_else(|| anyhow::anyhow!("Missing closing parameter tag"))?;

                        let value = line[content_start..content_end].trim().to_string();
                        debug!("Single-line parameter: {} = {}", current_param, value);
                        params.insert(current_param.clone(), value);
                        current_param.clear();
                    } else {
                        current_value.clear(); // Start collecting multi-line value
                    }
                }
                continue;
            }
        }

        // Check for parameter end with prefix
        if let Some(end_tag) = line.strip_prefix(&format!("</{}", PARAM_TAG_PREFIX)) {
            if end_tag
                .strip_suffix('>')
                .map_or(false, |name| name == current_param)
            {
                debug!("Parameter complete: {} = {}", current_param, current_value);
                params.insert(current_param.clone(), current_value.trim().to_string());
                current_param.clear();
                current_value.clear();
            }
            continue;
        }

        // If we're inside a parameter, collect its value
        if !current_param.is_empty() {
            if !current_value.is_empty() {
                current_value.push('\n');
            }
            current_value.push_str(line);
            debug!("Added value content: {}", current_value);
        }
    }

    debug!("Final parameters: {:?}", params);
    parse_tool_from_params(&tool_name, &params)
}

pub fn parse_tool_from_params(tool_name: &str, params: &HashMap<String, String>) -> Result<Tool> {
    match tool_name {
        "search_files" => Ok(Tool::SearchFiles {
            query: params
                .get("query")
                .ok_or_else(|| anyhow::anyhow!("Missing query"))?
                .to_string(),
            path: params.get("path").map(PathBuf::from),
            case_sensitive: params.get("case_sensitive").map_or(false, |v| v == "true"),
            whole_words: params.get("whole_words").map_or(false, |v| v == "true"),
            regex_mode: params.get("regex_mode").map_or(false, |v| v == "true"),
            max_results: params
                .get("max_results")
                .map(|v| v.trim().parse::<usize>())
                .transpose()?,
        }),

        "list_files" => Ok(Tool::ListFiles {
            paths: params
                .get("paths")
                .ok_or_else(|| anyhow::anyhow!("Missing paths"))?
                .split(',')
                .map(|s| PathBuf::from(s.trim()))
                .collect(),
            max_depth: params
                .get("max_depth")
                .map(|v| v.trim().parse::<usize>())
                .transpose()?,
        }),

        "read_files" => Ok(Tool::ReadFiles {
            paths: params
                .get("paths")
                .ok_or_else(|| anyhow::anyhow!("Missing paths"))?
                .split(',')
                .map(|s| PathBuf::from(s.trim()))
                .collect(),
        }),

        "summarize" => Ok(Tool::Summarize {
            files: params
                .get("files")
                .ok_or_else(|| anyhow::anyhow!("Missing files"))?
                .lines()
                .filter_map(|line| {
                    let mut parts = line.splitn(2, ':');
                    Some((
                        PathBuf::from(parts.next()?.trim()),
                        parts.next()?.trim().to_string(),
                    ))
                })
                .collect(),
        }),

        "update_file" => Ok(Tool::UpdateFile {
            path: PathBuf::from(
                params
                    .get("path")
                    .ok_or_else(|| anyhow::anyhow!("Missing path"))?,
            ),
            updates: params
                .get("updates")
                .ok_or_else(|| anyhow::anyhow!("Missing updates"))?
                .lines()
                .filter_map(|line| {
                    let mut parts = line.split(',');
                    Some(FileUpdate {
                        start_line: parts.next()?.trim().parse().ok()?,
                        end_line: parts.next()?.trim().parse().ok()?,
                        new_content: parts.next()?.trim().to_string(),
                    })
                })
                .collect(),
        }),

        "write_file" => Ok(Tool::WriteFile {
            path: PathBuf::from(
                params
                    .get("path")
                    .ok_or_else(|| anyhow::anyhow!("Missing path"))?,
            ),
            content: params
                .get("content")
                .ok_or_else(|| anyhow::anyhow!("Missing content"))?
                .to_string(),
        }),

        "delete_files" => Ok(Tool::DeleteFiles {
            paths: params
                .get("paths")
                .ok_or_else(|| anyhow::anyhow!("Missing paths"))?
                .split(',')
                .map(|s| PathBuf::from(s.trim()))
                .collect(),
        }),

        "ask_user" => Ok(Tool::AskUser {
            question: params
                .get("question")
                .ok_or_else(|| anyhow::anyhow!("Missing question"))?
                .to_string(),
        }),

        "message_user" => Ok(Tool::MessageUser {
            message: params
                .get("message")
                .ok_or_else(|| anyhow::anyhow!("Missing message"))?
                .to_string(),
        }),

        "complete_task" => Ok(Tool::CompleteTask {
            message: params
                .get("message")
                .ok_or_else(|| anyhow::anyhow!("Missing message"))?
                .to_string(),
        }),

        "execute_command" => Ok(Tool::ExecuteCommand {
            command_line: params
                .get("command_line")
                .ok_or_else(|| anyhow::anyhow!("Missing command_line"))?
                .to_string(),
            working_dir: params.get("working_dir").map(PathBuf::from),
        }),

        _ => Err(anyhow::anyhow!("Unknown tool: {}", tool_name)),
    }
}

pub fn parse_tool_json(name: &str, params: &serde_json::Value) -> Result<Tool> {
    match name {
        "execute_command" => Ok(Tool::ExecuteCommand {
            command_line: params["command_line"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing command_line"))?
                .to_string(),
            working_dir: params
                .get("working_dir")
                .and_then(|d| d.as_str())
                .map(PathBuf::from),
        }),
        "search_files" => Ok(Tool::SearchFiles {
            query: params["query"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing query"))?
                .to_string(),
            path: params
                .get("path")
                .and_then(|p| p.as_str())
                .map(PathBuf::from),
            case_sensitive: params
                .get("case_sensitive")
                .and_then(|b| b.as_bool())
                .unwrap_or(false),
            whole_words: params
                .get("whole_words")
                .and_then(|b| b.as_bool())
                .unwrap_or(false),
            regex_mode: params
                .get("mode")
                .and_then(|m| m.as_str())
                .map_or(false, |m| m == "regex"),
            max_results: params
                .get("max_results")
                .and_then(|n| n.as_u64())
                .map(|n| n as usize),
        }),
        "list_files" => Ok(Tool::ListFiles {
            paths: params["paths"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing or invalid paths array"))?
                .iter()
                .map(|p| {
                    Ok(PathBuf::from(
                        p.as_str()
                            .ok_or_else(|| anyhow::anyhow!("Invalid path in array"))?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
            max_depth: params["max_depth"].as_u64().map(|d| d as usize),
        }),
        "read_files" => Ok(Tool::ReadFiles {
            paths: params["paths"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing or invalid paths array"))?
                .iter()
                .map(|p| {
                    Ok(PathBuf::from(
                        p.as_str()
                            .ok_or_else(|| anyhow::anyhow!("Invalid path in array"))?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
        }),
        "summarize" => Ok(Tool::Summarize {
            files: params["files"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing or invalid files array"))?
                .iter()
                .map(|f| {
                    Ok((
                        PathBuf::from(
                            f["path"]
                                .as_str()
                                .ok_or_else(|| anyhow::anyhow!("Missing path in file entry"))?,
                        ),
                        f["summary"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("Missing summary in file entry"))?
                            .to_string(),
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
        }),
        "update_file" => Ok(Tool::UpdateFile {
            path: PathBuf::from(
                params["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?,
            ),
            updates: params["updates"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing or invalid updates array"))?
                .iter()
                .map(|update| {
                    Ok(FileUpdate {
                        start_line: update["start_line"]
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("Invalid or missing start_line"))?
                            as usize,
                        end_line: update["end_line"]
                            .as_u64()
                            .ok_or_else(|| anyhow::anyhow!("Invalid or missing end_line"))?
                            as usize,
                        new_content: update["new_content"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("Missing new_content"))?
                            .to_string(),
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        }),
        "write_file" => Ok(Tool::WriteFile {
            path: PathBuf::from(
                params["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?,
            ),
            content: params["content"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing content parameter"))?
                .to_string(),
        }),
        "delete_files" => Ok(Tool::DeleteFiles {
            paths: params["paths"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing or invalid paths array"))?
                .iter()
                .map(|p| {
                    Ok(PathBuf::from(
                        p.as_str()
                            .ok_or_else(|| anyhow::anyhow!("Invalid path in array"))?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
        }),
        "ask_user" => Ok(Tool::AskUser {
            question: params["question"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing question parameter"))?
                .to_string(),
        }),
        "message_user" => Ok(Tool::MessageUser {
            message: params["message"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing message parameter"))?
                .to_string(),
        }),
        "complete_task" => Ok(Tool::CompleteTask {
            message: params["message"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing message parameter"))?
                .to_string(),
        }),
        _ => Err(anyhow::anyhow!("Unknown tool: {}", name)),
    }
}
