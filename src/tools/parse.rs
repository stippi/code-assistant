use crate::types::FileReplacement;
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

    let mut params: HashMap<String, Vec<String>> = HashMap::new();
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
                        params.entry(current_param.clone()).or_default().push(value);
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
                params
                    .entry(current_param.clone())
                    .or_default()
                    .push(current_value.trim().to_string());
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

fn parse_search_replace_blocks(content: &str) -> Result<Vec<FileReplacement>> {
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
) -> Result<Tool> {
    match tool_name {
        "search_files" => Ok(Tool::SearchFiles {
            query: params
                .get("query")
                .ok_or_else(|| anyhow::anyhow!("Missing query"))?
                .first()
                .ok_or_else(|| anyhow::anyhow!("Query parameter is empty"))?
                .to_string(),
            path: params
                .get("path")
                .and_then(|v| v.first())
                .map(PathBuf::from),
            case_sensitive: params
                .get("case_sensitive")
                .map_or(false, |v| v.first().map_or(false, |s| s == "true")),
            whole_words: params
                .get("whole_words")
                .map_or(false, |v| v.first().map_or(false, |s| s == "true")),
            regex_mode: params
                .get("regex_mode")
                .map_or(false, |v| v.first().map_or(false, |s| s == "true")),
            max_results: params
                .get("max_results")
                .and_then(|v| v.first())
                .map(|v| v.trim().parse::<usize>())
                .transpose()?,
        }),

        "list_files" => Ok(Tool::ListFiles {
            paths: params
                .get("path")
                .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?
                .iter()
                .map(|s| PathBuf::from(s.trim()))
                .collect(),
            max_depth: params
                .get("max_depth")
                .and_then(|v| v.first())
                .map(|v| v.trim().parse::<usize>())
                .transpose()?,
        }),

        "read_files" => Ok(Tool::ReadFiles {
            paths: params
                .get("path")
                .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?
                .iter()
                .map(|s| PathBuf::from(s.trim()))
                .collect(),
        }),

        "summarize" => Ok(Tool::Summarize {
            files: params
                .get("file")
                .ok_or_else(|| anyhow::anyhow!("Missing file parameter"))?
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

        "replace_in_file" => Ok(Tool::ReplaceInFile {
            path: PathBuf::from(
                params
                    .get("path")
                    .and_then(|v| v.first())
                    .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?,
            ),
            replacements: parse_search_replace_blocks(
                params
                    .get("diff")
                    .and_then(|v| v.first())
                    .ok_or_else(|| anyhow::anyhow!("Missing diff parameter"))?,
            )?,
        }),

        "write_file" => Ok(Tool::WriteFile {
            path: PathBuf::from(
                params
                    .get("path")
                    .and_then(|v| v.first())
                    .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?,
            ),
            content: params
                .get("content")
                .and_then(|v| v.first())
                .ok_or_else(|| anyhow::anyhow!("Missing content parameter"))?
                .to_string(),
        }),

        "delete_files" => Ok(Tool::DeleteFiles {
            paths: params
                .get("path")
                .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?
                .iter()
                .map(|s| PathBuf::from(s.trim()))
                .collect(),
        }),

        "ask_user" => Ok(Tool::AskUser {
            question: params
                .get("question")
                .ok_or_else(|| anyhow::anyhow!("Missing question parameter"))?
                .first()
                .ok_or_else(|| anyhow::anyhow!("Question parameter is empty"))?
                .to_string(),
        }),

        "message_user" => Ok(Tool::MessageUser {
            message: params
                .get("message")
                .ok_or_else(|| anyhow::anyhow!("Missing message parameter"))?
                .first()
                .ok_or_else(|| anyhow::anyhow!("Message parameter is empty"))?
                .to_string(),
        }),

        "complete_task" => Ok(Tool::CompleteTask {
            message: params
                .get("message")
                .ok_or_else(|| anyhow::anyhow!("Missing message parameter"))?
                .first()
                .ok_or_else(|| anyhow::anyhow!("Message parameter is empty"))?
                .to_string(),
        }),

        "execute_command" => Ok(Tool::ExecuteCommand {
            command_line: params
                .get("command_line")
                .ok_or_else(|| anyhow::anyhow!("Missing command_line parameter"))?
                .first()
                .ok_or_else(|| anyhow::anyhow!("Command line parameter is empty"))?
                .to_string(),
            working_dir: params
                .get("working_dir")
                .and_then(|v| v.first())
                .map(|v| PathBuf::from(v)),
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
        "replace_in_file" => Ok(Tool::ReplaceInFile {
            path: PathBuf::from(
                params["path"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing path parameter"))?,
            ),
            replacements: params["replacements"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Missing replacements array"))?
                .iter()
                .map(|r| {
                    Ok(FileReplacement {
                        search: r["search"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("Missing search content"))?
                            .to_string(),
                        replace: r["replace"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("Missing replace content"))?
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
