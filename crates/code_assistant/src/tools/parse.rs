use crate::tools::core::ToolRegistry;
use crate::tools::tool_use_filter::ToolUseFilter;
use crate::tools::ToolRequest;
use crate::types::ToolError;
use anyhow::{anyhow, Result};
use fs_explorer::FileReplacement;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, trace};

const TOOL_TAG_PREFIX: &str = "tool:";
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
                            ToolError::ParseError(format!("Invalid start line number: {start}"))
                        })?)
                    };

                    let end_line = if end.is_empty() {
                        None // file.txt:10-
                    } else {
                        Some(end.parse::<usize>().map_err(|_| {
                            ToolError::ParseError(format!("Invalid end line number: {end}"))
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
                        ToolError::ParseError(format!("Invalid line number: {line_range}"))
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

#[derive(Debug, Clone)]
enum ParseState {
    SearchingForTool,
    InTool {
        tool_name: String,
        start_pos: usize,
    },
    InParam {
        tool_name: String,
        param_name: String,
        start_pos: usize,
    },
}

pub fn parse_caret_tool_invocations(
    text: &str,
    request_id: u64,
    _start_tool_count: usize,
    filter: Option<&dyn ToolUseFilter>,
) -> Result<(Vec<ToolRequest>, String)> {
    let mut tool_requests = Vec::new();
    let tool_regex = regex::Regex::new(r"(?m)^\^\^\^([a-zA-Z0-9_]+)$").unwrap();
    let multiline_start_regex = regex::Regex::new(r"(?m)^([a-zA-Z0-9_]+)\s+---\s*$").unwrap();
    let multiline_end_regex = regex::Regex::new(r"(?m)^---\s+([a-zA-Z0-9_]+)\s*$").unwrap();
    let tool_end_regex = regex::Regex::new(r"(?m)^\^\^\^$").unwrap();

    let mut remaining_text = text;
    let mut truncation_pos = text.len(); // Default to end of text
    let mut current_offset = 0; // Track position in original text

    while let Some(tool_match) = tool_regex.find(remaining_text) {
        // Calculate absolute positions in original text
        let tool_start_abs = current_offset + tool_match.start();

        // Extract tool name
        let tool_name = tool_regex
            .captures(&remaining_text[tool_match.start()..])
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str())
            .ok_or_else(|| ToolError::ParseError("Failed to extract tool name".to_string()))?;

        // Check filter before processing this tool
        let tool_index = tool_requests.len() + 1;
        if let Some(filter) = filter {
            if !filter.allow_tool_at_position(tool_name, tool_index) {
                // Tool not allowed, keep existing truncation_pos
                break;
            }
        }

        // Check if the tool exists in the registry
        if ToolRegistry::global().get(tool_name).is_none() {
            return Err(ToolError::UnknownTool(tool_name.to_string()).into());
        }

        // Find the end of this tool block
        let tool_start = tool_match.end();
        let remaining_after_tool = &remaining_text[tool_start..];

        let (tool_content, tool_end_pos, tool_end_abs) =
            if let Some(end_match) = tool_end_regex.find(remaining_after_tool) {
                let content = &remaining_after_tool[..end_match.start()];
                let end_pos = tool_start + end_match.end();
                let end_abs = current_offset + end_pos;
                (content, Some(end_pos), end_abs)
            } else {
                // No end found, treat rest as tool content
                let end_abs = text.len();
                (remaining_after_tool, None, end_abs)
            };

        // Parse parameters from tool content (returns raw HashMap)
        let raw_params = parse_caret_tool_parameters_raw(
            tool_content,
            &multiline_start_regex,
            &multiline_end_regex,
        )?;

        // Convert parameters to JSON using schema-based approach if tool exists, otherwise use fallback
        let tool_params = if ToolRegistry::global().get(tool_name).is_some() {
            // Use schema-based conversion for registered tools
            convert_xml_params_to_json(tool_name, &raw_params, ToolRegistry::global()).map_err(
                |e| {
                    ToolError::ParseError(format!("Error converting caret parameters to JSON: {e}"))
                },
            )?
        } else {
            // Fallback to legacy conversion for unregistered tools (mainly for tests)
            convert_raw_params_to_json_fallback(&raw_params)
        };

        // Generate a unique tool ID: tool-<request_id>-<tool_index_in_request>
        let tool_id = format!("tool-{request_id}-{tool_index}");

        // Create a ToolRequest with offset information
        let tool_request = ToolRequest {
            id: tool_id,
            name: tool_name.to_string(),
            input: tool_params,
            start_offset: Some(tool_start_abs),
            end_offset: Some(tool_end_abs),
        };

        tool_requests.push(tool_request);

        // Update remaining text for next iteration
        if let Some(end_pos) = tool_end_pos {
            // Always update truncation position after each successfully processed tool
            // This ensures we can truncate cleanly after the last allowed tool
            let absolute_tool_end = current_offset + end_pos;
            truncation_pos = absolute_tool_end;

            // Check if we should allow content after this tool
            if let Some(filter) = filter {
                if !filter.allow_content_after_tool(tool_name, tool_index) {
                    // No content allowed after this tool, truncate here
                    break;
                }
            }

            current_offset += end_pos;
            remaining_text = &remaining_text[end_pos..];
        } else {
            // No tool end found, stop processing
            break;
        }
    }

    // Return the parsed tools and truncated text
    let truncated_text = &text[..truncation_pos];
    Ok((tool_requests, truncated_text.to_string()))
}

fn parse_caret_tool_parameters_raw(
    content: &str,
    multiline_start_regex: &regex::Regex,
    multiline_end_regex: &regex::Regex,
) -> Result<HashMap<String, Vec<String>>> {
    let mut params: HashMap<String, Vec<String>> = HashMap::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();

        if line.is_empty() {
            i += 1;
            continue;
        }

        // Check for simple key: value parameters
        if let Some((key, value)) = parse_simple_caret_parameter(line) {
            // Handle array syntax: key: [
            if value == "[" {
                let mut array_content = Vec::new();
                i += 1;

                // Collect array elements until ]
                while i < lines.len() {
                    let array_line = lines[i].trim();
                    if array_line == "]" {
                        break;
                    }
                    if !array_line.is_empty() {
                        array_content.push(array_line.to_string());
                    }
                    i += 1;
                }

                params.insert(key, array_content);
            } else {
                params.entry(key).or_default().push(value);
            }
            i += 1;
            continue;
        }

        // Check for multiline parameter start
        if let Some(caps) = multiline_start_regex.captures(line) {
            if let Some(param_name) = caps.get(1) {
                let mut multiline_content = String::new();
                i += 1;

                // Collect content until end marker
                while i < lines.len() {
                    if let Some(end_caps) = multiline_end_regex.captures(lines[i]) {
                        if let Some(end_param) = end_caps.get(1) {
                            if end_param.as_str() == param_name.as_str() {
                                break;
                            }
                        }
                    }

                    if !multiline_content.is_empty() {
                        multiline_content.push('\n');
                    }
                    multiline_content.push_str(lines[i]);
                    i += 1;
                }

                params
                    .entry(param_name.as_str().to_string())
                    .or_default()
                    .push(multiline_content);
            }
            i += 1;
            continue;
        }

        i += 1;
    }

    Ok(params)
}

/// Convert raw parameters to JSON without schema validation (fallback for unregistered tools)
fn convert_raw_params_to_json_fallback(params: &HashMap<String, Vec<String>>) -> Value {
    let mut result = serde_json::Map::new();
    for (key, values) in params {
        if values.len() == 1 {
            result.insert(key.clone(), Value::String(values[0].clone()));
        } else if !values.is_empty() {
            result.insert(
                key.clone(),
                Value::Array(values.iter().map(|v| Value::String(v.clone())).collect()),
            );
        }
    }
    Value::Object(result)
}

fn parse_simple_caret_parameter(line: &str) -> Option<(String, String)> {
    if let Some((key, value)) = line.split_once(':') {
        let key = key.trim();
        let value = value.trim();

        // Skip lines that look like multiline markers
        if value == "---" || key.is_empty() {
            return None;
        }

        Some((key.to_string(), value.to_string()))
    } else {
        None
    }
}

fn parse_tool_xml(xml: &str) -> Result<(String, Value), ToolError> {
    trace!("Parsing XML:\n{}", xml);

    let tool_name = xml
        .trim()
        .strip_prefix(&format!("<{TOOL_TAG_PREFIX}"))
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.strip_suffix('>'))
        .ok_or_else(|| ToolError::ParseError("Missing tool name".into()))?
        .to_string();

    trace!("Found tool name: {}", tool_name);

    let mut params: HashMap<String, Vec<String>> = HashMap::new();
    let mut current_param = String::new();
    let mut content_start = 0;

    let chars = xml.char_indices().peekable();
    for (i, ch) in chars {
        if ch == '<' {
            // Check for parameter tag
            let rest = &xml[i..];
            trace!("Found '<', rest of string: {}", rest);
            if rest.starts_with(&format!("</{PARAM_TAG_PREFIX}")) {
                // Closing tag
                let param_name = rest[format!("</{PARAM_TAG_PREFIX}").len()..] // skip the "</param:"
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
            } else if let Some(param_start) = rest.strip_prefix(&format!("<{PARAM_TAG_PREFIX}")) {
                // Opening tag
                if let Some(param_name) = param_start.split('>').next() {
                    current_param = param_name.to_string();
                    content_start = i + format!("<{PARAM_TAG_PREFIX}{param_name}>").len();
                    trace!("Found param start: {} at {}", current_param, content_start);
                }
            }
        }
    }

    trace!("Final parameters: {:?}", params);

    // Convert parameters to JSON using the ToolRegistry
    let json_params = convert_xml_params_to_json(&tool_name, &params, ToolRegistry::global())
        .map_err(|e| ToolError::ParseError(format!("Error converting parameters to JSON: {e}")))?;

    Ok((tool_name, json_params))
}

pub(crate) fn parse_search_replace_blocks(
    content: &str,
) -> Result<Vec<FileReplacement>, ToolError> {
    let mut replacements = Vec::new();
    let mut lines = content.lines().peekable();
    let mut had_valid_block = false;

    // Skip leading empty lines
    while let Some(line) = lines.peek() {
        if line.trim().is_empty() {
            lines.next();
        } else {
            break;
        }
    }

    // Check first non-empty line is a start marker
    if let Some(line) = lines.peek() {
        let trimmed = line.trim_end();
        if !trimmed.starts_with("<<<<<<< SEARCH") {
            return Err(ToolError::ParseError(
                "Malformed diff: Unexpected content before diff markers".to_string(),
            ));
        }
    } else {
        // Empty content
        return Err(ToolError::ParseError(
            "Malformed diff: No search/replace blocks found. Expecting content to start with <<<<<<< SEARCH".to_string(),
        ));
    }

    while let Some(line) = lines.next() {
        // Skip empty lines between blocks
        if line.trim().is_empty() {
            continue;
        }

        // Match the exact marker without trimming leading whitespace
        let is_search_all = line.trim_end() == "<<<<<<< SEARCH_ALL";
        let is_search = line.trim_end() == "<<<<<<< SEARCH";

        if is_search || is_search_all {
            had_valid_block = true;
            let mut search = String::new();
            let mut replace = String::new();
            let mut found_separator = false;

            // Collect search content until we find the separator
            for line in lines.by_ref() {
                if line.trim_end() == "=======" {
                    found_separator = true;
                    break;
                }
                if !search.is_empty() {
                    search.push('\n');
                }
                search.push_str(line);
            }

            if !found_separator {
                return Err(ToolError::ParseError(
                    "Malformed diff: Missing separator marker (=======)".to_string(),
                ));
            }

            // Collect replace content
            let end_marker = if is_search_all {
                ">>>>>>> REPLACE_ALL"
            } else {
                ">>>>>>> REPLACE"
            };
            let mut found_end_marker = false;

            // Before collecting the replace content, we'll check if there are
            // additional separator markers in the remaining content
            {
                // Clone the iterator to peek ahead without consuming it
                let preview_iter = lines.clone();
                let mut lines_to_end_marker = Vec::new();
                let mut reached_end_marker = false;

                // Collect all lines until end marker
                for line in preview_iter {
                    if line.trim_end() == end_marker {
                        reached_end_marker = true;
                        break;
                    }
                    lines_to_end_marker.push(line);
                }

                if !reached_end_marker {
                    return Err(ToolError::ParseError(
                        "Malformed diff: Missing closing marker".to_string(),
                    ));
                }

                // Check for invalid separators
                let separator_count = lines_to_end_marker
                    .iter()
                    .filter(|line| line.trim_end() == "=======")
                    .count();

                // Special case: allow one separator if it's the last line before end marker
                if separator_count > 0 {
                    let last_line = lines_to_end_marker.last();

                    if separator_count > 1
                        || (last_line.is_some_and(|line| line.trim_end() != "======="))
                    {
                        return Err(ToolError::ParseError(
                            "Malformed diff: Multiple separator markers (=======) found in the content. This is not allowed as it would make it impossible to edit files containing separators.".to_string(),
                        ));
                    }
                }
            }

            // Now actually process the replace content
            while let Some(current_line) = lines.next() {
                // Check for end marker
                if current_line.trim_end() == end_marker {
                    found_end_marker = true;
                    break;
                }

                // Check if this is a separator right before end marker
                if current_line.trim_end() == "=======" {
                    if let Some(next_line) = lines.peek() {
                        if next_line.trim_end() == end_marker {
                            // Skip this separator if it's right before the end marker
                            continue;
                        }
                    }

                    // This should never happen due to our check above, but just in case
                    return Err(ToolError::ParseError(
                        "Malformed diff: Found separator marker (=======) in replace content. This is not allowed as it would make subsequent edits impossible.".to_string(),
                    ));
                }

                // Regular content line - add to replace content
                if !replace.is_empty() {
                    replace.push('\n');
                }
                replace.push_str(current_line);
            }

            if !found_end_marker {
                return Err(ToolError::ParseError(
                    "Malformed diff: Missing closing marker (>>>>>>> REPLACE)".to_string(),
                ));
            }

            replacements.push(FileReplacement {
                search,
                replace,
                replace_all: is_search_all,
            });
        } else {
            // Found a non-empty line that isn't a start marker
            return Err(ToolError::ParseError(
                "Malformed diff: Unexpected content between diff blocks".to_string(),
            ));
        }
    }

    // Check for non-whitespace content after all blocks are processed
    for line in lines {
        if !line.trim().is_empty() {
            return Err(ToolError::ParseError(
                "Malformed diff: Unexpected content after diff blocks".to_string(),
            ));
        }
    }

    if !had_valid_block {
        return Err(ToolError::ParseError(
            "Malformed diff: No valid search/replace blocks found".to_string(),
        ));
    }

    Ok(replacements)
}

/// Convert XML parameter HashMap to JSON Value based on tool schema
pub fn convert_xml_params_to_json(
    tool_name: &str,
    params: &HashMap<String, Vec<String>>,
    registry: &ToolRegistry,
) -> Result<Value> {
    // Get tool schema from registry
    let tool = registry
        .get(tool_name)
        .ok_or_else(|| anyhow!("Tool {tool_name} not found in registry"))?;

    let tool_spec = tool.spec();
    let schema = &tool_spec.parameters_schema;

    // Create base object
    let mut result = json!({});
    let mut processed_params = std::collections::HashSet::new();

    // Access properties from schema
    if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
        for (prop_name, prop_schema) in properties {
            // Determine type from schema
            let prop_type = prop_schema
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("string");

            match prop_type {
                // For arrays - handle both singular and plural forms
                "array" => {
                    // First try exact property name (including empty arrays)
                    let values_exact = params.get(prop_name);

                    // If not found, try alternative singular/plural form
                    let (values, used_param_name) = if values_exact.is_none() {
                        // Get alternative name (singular/plural form)
                        let alt_name = if prop_name.ends_with('s') {
                            // Remove the 's' at the end for singular form
                            prop_name[0..prop_name.len() - 1].to_string()
                        } else {
                            // Add 's' for plural form
                            format!("{prop_name}s")
                        };

                        if let Some(alt_values) = params.get(&alt_name) {
                            (Some(alt_values), alt_name)
                        } else {
                            (None, prop_name.clone())
                        }
                    } else {
                        (values_exact, prop_name.clone())
                    };

                    // If we have values (including empty arrays), add them as an array
                    if let Some(array_values) = values {
                        result[prop_name] = json!(array_values);
                        processed_params.insert(used_param_name);
                    }
                }
                // For all other types, use normal parameter handling
                _ => {
                    // Skip if parameter not provided
                    if !params.contains_key(prop_name) {
                        continue;
                    }

                    // Get the parameter values
                    let param_values = &params[prop_name];
                    if param_values.is_empty() {
                        continue;
                    }

                    match prop_type {
                        // For boolean convert from string
                        "boolean" => {
                            let bool_value = param_values[0].to_lowercase() == "true";
                            result[prop_name] = json!(bool_value);
                        }
                        // For numbers convert from string
                        "number" => {
                            if let Ok(num) = param_values[0].parse::<f64>() {
                                result[prop_name] = json!(num);
                            } else {
                                return Err(anyhow!(
                                    "Failed to parse '{}' as number for parameter '{}'",
                                    param_values[0],
                                    prop_name
                                ));
                            }
                        }
                        // For integers convert from string
                        "integer" => {
                            if let Ok(num) = param_values[0].parse::<i64>() {
                                result[prop_name] = json!(num);
                            } else {
                                return Err(anyhow!(
                                    "Failed to parse '{}' as integer for parameter '{}'",
                                    param_values[0],
                                    prop_name
                                ));
                            }
                        }
                        // Default to string (first value only)
                        _ => {
                            result[prop_name] = json!(param_values[0]);
                        }
                    }
                    processed_params.insert(prop_name.clone());
                }
            }
        }
    }

    // Add any unprocessed parameters as-is (for backward compatibility with tests)
    for (param_name, param_values) in params {
        if !processed_params.contains(param_name) && !param_values.is_empty() {
            if param_values.len() == 1 {
                result[param_name] = json!(param_values[0]);
            } else {
                result[param_name] = json!(param_values);
            }
        }
    }

    Ok(result)
}

pub fn parse_xml_tool_invocations(
    text: &str,
    request_id: u64,
    _start_tool_count: usize,
    filter: Option<&dyn ToolUseFilter>,
) -> Result<(Vec<ToolRequest>, String)> {
    let mut tool_requests = Vec::new();
    let mut state = ParseState::SearchingForTool;
    let mut current_pos = 0;
    let mut truncation_pos = text.len(); // Default to end of text

    while current_pos < text.len() {
        // Find next '<' character
        if let Some(tag_start) = text[current_pos..].find('<') {
            let abs_tag_start = current_pos + tag_start;

            // Extract tag content until '>'
            if let Some(tag_end) = text[abs_tag_start..].find('>') {
                let abs_tag_end = abs_tag_start + tag_end;
                let tag_content = &text[abs_tag_start + 1..abs_tag_end]; // Skip '<' and '>'

                debug!("Found tag: <{}>", tag_content);

                // Check if this is a tool tag
                if let Some(tool_name) = tag_content.strip_prefix(TOOL_TAG_PREFIX) {
                    match &state {
                        ParseState::SearchingForTool => {
                            // Check filter before starting this tool
                            let tool_index = tool_requests.len() + 1;
                            if let Some(filter) = filter {
                                if !filter.allow_tool_at_position(tool_name, tool_index) {
                                    // Tool not allowed at this position, keep existing truncation_pos
                                    break;
                                }
                            }

                            // Start of a new tool invocation
                            state = ParseState::InTool {
                                tool_name: tool_name.to_string(),
                                start_pos: abs_tag_start,
                            };
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                        ParseState::InTool {
                            tool_name: current_tool,
                            ..
                        }
                        | ParseState::InParam {
                            tool_name: current_tool,
                            ..
                        } => {
                            // Found another tool start while already in a tool - this is an error
                            return Err(ToolError::ParseError(format!(
                                "Malformed tool invocation: found nested tool invocation. Started '{current_tool}' but found start of '{tool_name}' before closing the first one"
                            )).into());
                        }
                    }
                }
                // Check if this is a tool closing tag
                else if let Some(tool_name) =
                    tag_content.strip_prefix(&format!("/{TOOL_TAG_PREFIX}"))
                {
                    match &state {
                        ParseState::SearchingForTool => {
                            // Found closing tag without opening tag
                            return Err(ToolError::ParseError(format!(
                                "Malformed tool invocation: found closing tag '</tool:{tool_name}>' without corresponding opening tag"
                            )).into());
                        }
                        ParseState::InTool {
                            tool_name: current_tool,
                            start_pos,
                        } => {
                            if tool_name != current_tool {
                                // Mismatched closing tag
                                return Err(ToolError::ParseError(format!(
                                    "Malformed tool invocation: mismatching tool names in start and end tag. Expected '</tool:{current_tool}>' but found '</tool:{tool_name}>'"
                                )).into());
                            }

                            // Extract the complete tool XML
                            let tool_content = &text[*start_pos..abs_tag_end + 1];
                            debug!("Found complete tool content:\n{}", tool_content);

                            // Parse the tool XML to get tool name and parameters
                            let (parsed_tool_name, tool_params) = parse_tool_xml(tool_content)?;

                            // Check if the tool exists in the registry
                            if ToolRegistry::global().get(&parsed_tool_name).is_none() {
                                return Err(ToolError::UnknownTool(parsed_tool_name).into());
                            }

                            // Generate a unique tool ID: tool-<request_id>-<tool_index_in_request>
                            let tool_index = tool_requests.len() + 1;
                            let tool_id = format!("tool-{request_id}-{tool_index}");

                            // Create a ToolRequest with offset information
                            let tool_request = ToolRequest {
                                id: tool_id,
                                name: parsed_tool_name.clone(),
                                input: tool_params,
                                start_offset: Some(*start_pos),
                                end_offset: Some(abs_tag_end + 1),
                            };

                            tool_requests.push(tool_request);

                            // Always update truncation position after each successfully processed tool
                            // This ensures we can truncate cleanly after the last allowed tool
                            truncation_pos = abs_tag_end + 1;

                            // Check if we should allow content after this tool
                            if let Some(filter) = filter {
                                if !filter.allow_content_after_tool(&parsed_tool_name, tool_index) {
                                    // No content allowed after this tool, truncate here
                                    state = ParseState::SearchingForTool;
                                    break;
                                }
                            }

                            // Reset state
                            state = ParseState::SearchingForTool;
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                        ParseState::InParam {
                            param_name: current_param,
                            ..
                        } => {
                            // We're still inside a parameter when trying to close the tool - this is an error
                            return Err(ToolError::ParseError(format!(
                                "Malformed tool invocation: unclosed parameter '{current_param}' in tool '{tool_name}' - missing closing tag '</param:{current_param}>'"
                            )).into());
                        }
                    }
                }
                // Handle parameter tags and other logic same as original function...
                else if let Some(param_name) = tag_content.strip_prefix(PARAM_TAG_PREFIX) {
                    match &state {
                        ParseState::SearchingForTool => {
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                        ParseState::InTool {
                            tool_name,
                            start_pos,
                        } => {
                            state = ParseState::InParam {
                                tool_name: tool_name.clone(),
                                param_name: param_name.to_string(),
                                start_pos: *start_pos,
                            };
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                        ParseState::InParam {
                            tool_name,
                            param_name: current_param,
                            ..
                        } => {
                            return Err(ToolError::ParseError(format!(
                                "Malformed tool invocation: found nested parameter. Started parameter '{current_param}' in tool '{tool_name}' but found start of parameter '{param_name}' before closing the first one"
                            )).into());
                        }
                    }
                } else if let Some(param_name) =
                    tag_content.strip_prefix(&format!("/{PARAM_TAG_PREFIX}"))
                {
                    match &state {
                        ParseState::SearchingForTool | ParseState::InTool { .. } => {
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                        ParseState::InParam {
                            tool_name,
                            param_name: current_param,
                            start_pos,
                            ..
                        } => {
                            if param_name != current_param {
                                return Err(ToolError::ParseError(format!(
                                    "Malformed tool invocation: mismatching parameter names in start and end tag. Expected '</param:{current_param}>' but found '</param:{param_name}>' in tool '{tool_name}'"
                                )).into());
                            }

                            state = ParseState::InTool {
                                tool_name: tool_name.clone(),
                                start_pos: *start_pos,
                            };
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                    }
                } else {
                    match &state {
                        ParseState::InTool { tool_name, .. } => {
                            return Err(ToolError::ParseError(format!(
                                "Malformed tool invocation: found unexpected tag '<{tag_content}>' inside tool '{tool_name}'. Only parameter tags are allowed inside tool blocks"
                            )).into());
                        }
                        ParseState::InParam { .. } => {
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                        ParseState::SearchingForTool => {
                            current_pos = abs_tag_end + 1;
                            continue;
                        }
                    }
                }
            } else {
                current_pos = abs_tag_start + 1;
                continue;
            }
        } else {
            break;
        }
    }

    // Check if we ended in an incomplete state
    match state {
        ParseState::SearchingForTool => {
            // This is fine
        }
        ParseState::InTool { tool_name, .. } => {
            return Err(ToolError::ParseError(format!(
                "Malformed tool invocation: unclosed tool '{tool_name}' - missing closing tag '</tool:{tool_name}>'"
            ))
            .into());
        }
        ParseState::InParam {
            tool_name,
            param_name,
            ..
        } => {
            return Err(ToolError::ParseError(format!(
                "Malformed tool invocation: unclosed parameter '{param_name}' in tool '{tool_name}' - missing closing tag '</param:{param_name}>'"
            )).into());
        }
    }

    // Return the parsed tools and truncated text
    let truncated_text = &text[..truncation_pos];
    Ok((tool_requests, truncated_text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::super::parse::parse_search_replace_blocks;
    use super::*;
    use crate::tools::core::ResourcesTracker;
    use crate::tools::core::{Tool, ToolContext, ToolScope, ToolSpec};
    use crate::tools::impls::{ListProjectsTool, ReadFilesTool};
    use std::collections::HashMap;

    // Test tool to use for schema parsing tests
    struct TestTool;

    #[async_trait::async_trait]
    impl Tool for TestTool {
        type Input = TestInput;
        type Output = TestOutput;

        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "test_tool",
                description: "Test tool for parameter conversion",
                parameters_schema: json!({
                    "type": "object",
                    "properties": {
                        "string_param": {
                            "type": "string",
                            "description": "A string parameter"
                        },
                        "number_param": {
                            "type": "number",
                            "description": "A number parameter"
                        },
                        "integer_param": {
                            "type": "integer",
                            "description": "An integer parameter"
                        },
                        "boolean_param": {
                            "type": "boolean",
                            "description": "A boolean parameter"
                        },
                        "array_param": {
                            "type": "array",
                            "description": "An array parameter",
                            "items": {
                                "type": "string"
                            }
                        }
                    },
                    "required": ["string_param"]
                }),
                annotations: None,
                supported_scopes: &[ToolScope::McpServer],
                hidden: false,
                title_template: None,
            }
        }

        async fn execute<'a>(
            &self,
            _context: &mut ToolContext<'a>,
            _input: &mut Self::Input,
        ) -> Result<Self::Output> {
            unimplemented!()
        }
    }

    #[derive(serde::Deserialize, serde::Serialize)]
    struct TestInput {
        string_param: String,
        #[serde(default)]
        number_param: Option<f64>,
        #[serde(default)]
        integer_param: Option<i64>,
        #[serde(default)]
        boolean_param: Option<bool>,
        #[serde(default)]
        array_param: Option<Vec<String>>,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct TestOutput;

    impl crate::tools::core::Render for TestOutput {
        fn status(&self) -> String {
            "Test output".to_string()
        }

        fn render(&self, _tracker: &mut ResourcesTracker) -> String {
            "Test output rendered".to_string()
        }
    }

    impl crate::tools::core::ToolResult for TestOutput {
        fn is_success(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn test_convert_xml_params_to_json() {
        // Create a registry with our test tool
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(TestTool));

        // Create XML-style params
        let mut params = HashMap::new();
        params.insert("string_param".to_string(), vec!["test string".to_string()]);
        params.insert("number_param".to_string(), vec!["42.5".to_string()]);
        params.insert("integer_param".to_string(), vec!["42".to_string()]);
        params.insert("boolean_param".to_string(), vec!["true".to_string()]);
        params.insert(
            "array_param".to_string(),
            vec!["item1".to_string(), "item2".to_string()],
        );

        // Convert to JSON
        let json_params = convert_xml_params_to_json("test_tool", &params, &registry).unwrap();

        // Verify conversion results
        assert_eq!(json_params["string_param"], "test string");
        assert_eq!(json_params["number_param"], 42.5);
        assert_eq!(json_params["integer_param"], 42);
        assert_eq!(json_params["boolean_param"], true);
        assert!(json_params["array_param"].is_array());
        assert_eq!(json_params["array_param"][0], "item1");
        assert_eq!(json_params["array_param"][1], "item2");
    }

    #[tokio::test]
    async fn test_convert_xml_params_error_handling() {
        // Create a registry with our test tool
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(TestTool));

        // Create XML-style params with invalid number
        let mut params = HashMap::new();
        params.insert("string_param".to_string(), vec!["test string".to_string()]);
        params.insert("number_param".to_string(), vec!["not a number".to_string()]);

        // Conversion should fail with a descriptive error
        let result = convert_xml_params_to_json("test_tool", &params, &registry);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to parse 'not a number' as number"));

        // Test with invalid integer
        let mut params = HashMap::new();
        params.insert("string_param".to_string(), vec!["test string".to_string()]);
        params.insert("integer_param".to_string(), vec!["42.5".to_string()]);

        // Conversion should fail with a descriptive error
        let result = convert_xml_params_to_json("test_tool", &params, &registry);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to parse '42.5' as integer"));
    }

    #[tokio::test]
    async fn test_convert_xml_params_real_tools() {
        // Create registry with real tools
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(ListProjectsTool));
        registry.register(Box::new(ReadFilesTool));

        // Test list_projects (simple case)
        let empty_params = HashMap::new();
        let json_params =
            convert_xml_params_to_json("list_projects", &empty_params, &registry).unwrap();
        assert_eq!(json_params, json!({}));

        // Test read_files with singular "path" in params (XML style)
        let mut params = HashMap::new();
        params.insert("project".to_string(), vec!["test-project".to_string()]);
        params.insert(
            "path".to_string(),
            vec!["file1.txt".to_string(), "file2.txt".to_string()],
        );

        let json_params = convert_xml_params_to_json("read_files", &params, &registry).unwrap();
        assert_eq!(json_params["project"], "test-project");
        assert!(json_params["paths"].is_array()); // Should match "paths" (plural) from schema
        assert_eq!(json_params["paths"].as_array().unwrap().len(), 2);
    }

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
        assert!(!result[0].replace_all);
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
        assert!(result[0].replace_all);
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
        assert!(!result[0].replace_all);
        assert_eq!(result[1].search, "console.log(");
        assert_eq!(result[1].replace, "logger.debug(");
        assert!(result[1].replace_all);
    }

    #[test]
    fn test_parse_multiple_search_replace_blocks_whitespace() {
        let content = concat!(
            "\n",
            "<<<<<<< SEARCH\n",
            "function test() {\n",
            "=======\n",
            "function renamed() {\n",
            ">>>>>>> REPLACE\n",
            "\n",
            "<<<<<<< SEARCH\n",
            "console.log(\n",
            "=======\n",
            "logger.debug(\n",
            ">>>>>>> REPLACE",
            "\n",
        );

        let result = parse_search_replace_blocks(content).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].search, "function test() {");
        assert_eq!(result[0].replace, "function renamed() {");
        assert!(!result[0].replace_all);
        assert_eq!(result[1].search, "console.log(");
        assert_eq!(result[1].replace, "logger.debug(");
        assert!(!result[1].replace_all);
    }

    #[test]
    fn test_parse_malformed_diff_with_missing_closing_marker() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "        content to search\n",
            "=======\n",
            "        content to replace with\n",
            "======="
        );

        // The diff is malformed (no closing >>>>>>> marker), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(result.is_err(), "Expected an error for malformed diff");
        let error_message = result.unwrap_err().to_string();
        assert!(
            error_message.contains("Missing closing marker"),
            "Error should mention the missing closing marker: {error_message}"
        );
    }

    #[test]
    fn test_parse_malformed_diff_with_multiple_separators() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "        content to search\n",
            "=======\n",
            "        some more content to search\n",
            "=======\n",
            "        content to replace with\n",
            ">>>>>>> REPLACE\n",
        );

        // The diff is malformed (it has multiple separators), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(
            result.is_err(),
            "Expected an error for malformed diff with multiple separators"
        );
        let error_message = result.unwrap_err().to_string();

        assert!(
            error_message.contains("Multiple separator markers"),
            "Error should mention the problem with multiple separator markers: {error_message}"
        );
    }

    #[test]
    fn test_parse_malformed_diff_missing_start_marker() {
        let content = concat!(
            "Some regular content\n",
            "content to search\n",
            "=======\n",
            "content to replace with\n",
            ">>>>>>> REPLACE"
        );

        // The diff is malformed (no start <<<<<<< marker), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(result.is_err(), "Expected an error for malformed diff");
        let error_message = result.unwrap_err().to_string();
        assert!(
            error_message.contains("content before diff markers"),
            "Error should mention unexpected content: {error_message}"
        );
    }

    #[test]
    fn test_parse_malformed_diff_with_content_between_blocks() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "content to search\n",
            "=======\n",
            "content to replace with\n",
            ">>>>>>> REPLACE\n",
            "Unexpected content between blocks\n",
            "<<<<<<< SEARCH\n",
            "second search\n",
            "=======\n",
            "second replace\n",
            ">>>>>>> REPLACE"
        );

        // The diff is malformed (non-whitespace content between blocks), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(result.is_err(), "Expected an error for malformed diff");
        let error_message = result.unwrap_err().to_string();
        assert!(
            error_message.contains("Unexpected content between diff blocks"),
            "Error should mention unexpected content between blocks: {error_message}"
        );
    }

    #[test]
    fn test_parse_malformed_diff_with_content_after_last_block() {
        let content = concat!(
            "<<<<<<< SEARCH\n",
            "content to search\n",
            "=======\n",
            "content to replace with\n",
            ">>>>>>> REPLACE\n",
            "Unexpected content after the last block"
        );

        // The diff is malformed (non-whitespace content after last block), so the function should return an error
        let result = parse_search_replace_blocks(content);
        assert!(result.is_err(), "Expected an error for malformed diff");
        let error_message = result.unwrap_err().to_string();

        // With the current implementation, this is detected as content between blocks
        // since we don't distinguish between "after last block" and "between blocks"
        assert!(
            error_message.contains("Unexpected content between diff blocks"),
            "Error should mention unexpected content: {error_message}"
        );
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_simple() {
        let text = concat!("^^^list_projects\n", "^^^");

        let (result, _) = parse_caret_tool_invocations(text, 123, 0, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "list_projects");
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_multiline() {
        let text = concat!(
            "^^^write_file\n",
            "project: test\n",
            "path: test.txt\n",
            "content ---\n",
            "This is multiline content\n",
            "with several lines\n",
            "--- content\n",
            "^^^"
        );

        let (result, _) = parse_caret_tool_invocations(text, 123, 0, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "write_file");
        assert_eq!(result[0].input["project"], "test");
        assert_eq!(result[0].input["path"], "test.txt");
        assert!(result[0].input["content"]
            .as_str()
            .unwrap()
            .contains("multiline content"));
        assert!(result[0].input["content"]
            .as_str()
            .unwrap()
            .contains("several lines"));
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_array() {
        let text = concat!(
            "^^^read_files\n",
            "project: test\n",
            "paths: [\n",
            "src/main.rs\n",
            "Cargo.toml\n",
            "README.md\n",
            "]\n",
            "^^^"
        );

        let (result, _) = parse_caret_tool_invocations(text, 123, 0, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "read_files");
        assert_eq!(result[0].input["project"], "test");
        assert!(result[0].input["paths"].is_array());
        let paths_array = result[0].input["paths"].as_array().unwrap();
        assert_eq!(paths_array.len(), 3);
        assert_eq!(paths_array[0], "src/main.rs");
        assert_eq!(paths_array[1], "Cargo.toml");
        assert_eq!(paths_array[2], "README.md");
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_multiple_multiline() {
        let text = concat!(
            "^^^edit\n",
            "project: test\n",
            "path: test.rs\n",
            "old_text ---\n",
            "old code here\n",
            "--- old_text\n",
            "new_text ---\n",
            "new code here\n",
            "--- new_text\n",
            "^^^"
        );

        let (result, _) = parse_caret_tool_invocations(text, 123, 0, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "edit");
        assert_eq!(result[0].input["project"], "test");
        assert_eq!(result[0].input["path"], "test.rs");
        assert_eq!(result[0].input["old_text"], "old code here");
        assert_eq!(result[0].input["new_text"], "new code here");
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_with_text_before() {
        let text = concat!("I'll help you with that.\n\n", "^^^list_projects\n", "^^^");

        let (result, _) = parse_caret_tool_invocations(text, 123, 0, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "list_projects");
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_no_tools() {
        let text = "This is just plain text with no tools.";

        let (result, _) = parse_caret_tool_invocations(text, 123, 0, None).unwrap();
        assert_eq!(result.len(), 0);
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_unknown_tool() {
        let text = concat!("^^^unknown_tool\n", "param: value\n", "^^^");

        let result = parse_caret_tool_invocations(text, 123, 0, None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown tool: unknown_tool"));
    }

    #[test]
    fn test_parse_simple_caret_parameter() {
        assert_eq!(
            parse_simple_caret_parameter("key: value"),
            Some(("key".to_string(), "value".to_string()))
        );

        assert_eq!(
            parse_simple_caret_parameter("  project  :  test-project  "),
            Some(("project".to_string(), "test-project".to_string()))
        );

        // Should skip multiline markers
        assert_eq!(parse_simple_caret_parameter("content: ---"), None);
        assert_eq!(parse_simple_caret_parameter("content ---"), None);

        // Should skip lines without colon
        assert_eq!(parse_simple_caret_parameter("just text"), None);
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_array_vs_single_value() {
        // Test that single value parameters remain as strings with write_file (has single path param)
        let text_single = concat!(
            "^^^write_file\n",
            "project: test\n",
            "path: single-file.txt\n",
            "content: test content\n",
            "^^^"
        );

        let (result, _) = parse_caret_tool_invocations(text_single, 123, 0, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "write_file");
        assert_eq!(result[0].input["project"], "test");

        // Single path should be a string
        let path = &result[0].input["path"];
        assert!(path.is_string());
        assert_eq!(path.as_str().unwrap(), "single-file.txt");

        // Test that array parameters are always arrays, even with single element
        let text_array = concat!(
            "^^^read_files\n",
            "project: test\n",
            "paths: [\n",
            "single-file.txt\n",
            "]\n",
            "^^^"
        );

        let (result, _) = parse_caret_tool_invocations(text_array, 123, 0, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "read_files");
        assert_eq!(result[0].input["project"], "test");

        // Array with single element should still be an array
        let paths = &result[0].input["paths"];
        assert!(paths.is_array());
        let paths_array = paths.as_array().unwrap();
        assert_eq!(paths_array.len(), 1);
        assert_eq!(paths_array[0], "single-file.txt");
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_empty_and_whitespace_arrays() {
        // Test with different array scenarios for read_files paths parameter
        let text_empty = concat!(
            "^^^read_files\n",
            "project: test\n",
            "paths: [\n",
            "]\n",
            "^^^"
        );

        let (result, _) = parse_caret_tool_invocations(text_empty, 123, 0, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "read_files");

        // Empty array should be empty
        let empty_array = result[0].input["paths"].as_array().unwrap();
        assert_eq!(empty_array.len(), 0);

        // Test with whitespace-only and mixed content
        let text_mixed = concat!(
            "^^^read_files\n",
            "project: test\n",
            "paths: [\n",
            "file1.txt\n",
            "  \n",
            "file2.txt\n",
            "\t\n",
            "]\n",
            "^^^"
        );

        let (result, _) = parse_caret_tool_invocations(text_mixed, 123, 0, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "read_files");

        // Mixed array should only contain non-empty elements (whitespace trimmed)
        let mixed_array = result[0].input["paths"].as_array().unwrap();
        assert_eq!(mixed_array.len(), 2);
        assert_eq!(mixed_array[0], "file1.txt");
        assert_eq!(mixed_array[1], "file2.txt");
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_multiline_and_array_mix() {
        // Test a combination of array parameters and numeric parameters
        let text = concat!(
            "^^^list_files\n",
            "project: test\n",
            "paths: [\n",
            "src/main.rs\n",
            "docs/README.md\n",
            "]\n",
            "max_depth: 3\n",
            "^^^"
        );

        let (result, _) = parse_caret_tool_invocations(text, 123, 0, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "list_files");

        // Single parameters should be strings
        assert_eq!(result[0].input["project"].as_str().unwrap(), "test");

        // Array parameters should be arrays
        let paths = result[0].input["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], "src/main.rs");
        assert_eq!(paths[1], "docs/README.md");

        // Numeric parameters should be converted properly
        assert!(result[0].input["max_depth"].is_number());
        assert_eq!(result[0].input["max_depth"].as_u64().unwrap(), 3);
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_with_numeric_parameters() {
        // Test with list_files that has max_depth as integer parameter
        let text = concat!(
            "^^^list_files\n",
            "project: test\n",
            "paths: [\n",
            "src\n",
            "docs\n",
            "]\n",
            "max_depth: 3\n",
            "^^^"
        );

        let (result, _) = parse_caret_tool_invocations(text, 123, 0, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "list_files");

        // String parameters should remain strings
        assert_eq!(result[0].input["project"].as_str().unwrap(), "test");

        // Array parameters should be arrays
        let paths = result[0].input["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], "src");
        assert_eq!(paths[1], "docs");

        // Integer parameters should be converted to numbers
        assert!(result[0].input["max_depth"].is_number());
        assert_eq!(result[0].input["max_depth"].as_u64().unwrap(), 3);
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_with_offsets() {
        let text = concat!(
            "Here's some text before the tool.\n",
            "^^^list_files\n",
            "project: test\n",
            "paths: [\n",
            "src\n",
            "]\n",
            "^^^\n",
            "And some text after the tool."
        );

        let (tool_requests, truncated_text) =
            parse_caret_tool_invocations(text, 123, 0, None).unwrap();

        assert_eq!(tool_requests.len(), 1);
        let tool_request = &tool_requests[0];

        // Check that offsets are correctly set
        assert!(tool_request.start_offset.is_some());
        assert!(tool_request.end_offset.is_some());

        let start = tool_request.start_offset.unwrap();
        let end = tool_request.end_offset.unwrap();

        // The tool block should start after "Here's some text before the tool.\n"
        let expected_start = "Here's some text before the tool.\n".len();
        assert_eq!(start, expected_start);

        // Extract the tool block using the offsets
        let tool_block_text = &text[start..end];
        assert!(tool_block_text.starts_with("^^^list_files"));
        assert!(tool_block_text.ends_with("^^^"));

        // Verify the truncated text includes the tool
        assert!(truncated_text.contains("^^^list_files"));
        assert!(truncated_text.contains("^^^"));
    }

    #[tokio::test]
    async fn test_parse_xml_tool_invocations_with_offsets() {
        let text = concat!(
            "Some introductory text.\n",
            "<tool:read_files>\n",
            "<param:project>test-project</param:project>\n",
            "<param:path>file1.txt</param:path>\n",
            "<param:path>file2.txt</param:path>\n",
            "</tool:read_files>\n",
            "Some concluding text."
        );

        let (tool_requests, truncated_text) =
            parse_xml_tool_invocations(text, 456, 0, None).unwrap();

        assert_eq!(tool_requests.len(), 1);
        let tool_request = &tool_requests[0];

        // Check that offsets are correctly set
        assert!(tool_request.start_offset.is_some());
        assert!(tool_request.end_offset.is_some());

        let start = tool_request.start_offset.unwrap();
        let end = tool_request.end_offset.unwrap();

        // The tool block should start after "Some introductory text.\n"
        let expected_start = "Some introductory text.\n".len();
        assert_eq!(start, expected_start);

        // Extract the tool block using the offsets
        let tool_block_text = &text[start..end];
        assert!(tool_block_text.starts_with("<tool:read_files>"));
        assert!(tool_block_text.ends_with("</tool:read_files>"));

        // Verify the truncated text includes the tool
        assert!(truncated_text.contains("<tool:read_files>"));
        assert!(truncated_text.contains("</tool:read_files>"));
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_tool_id_format() {
        let text = concat!(
            "^^^list_files\n",
            "project: test\n",
            "paths: [\n",
            "src\n",
            "]\n",
            "^^^"
        );

        // Test with specific request_id (start_tool_count is not used anymore)
        let request_id = 456;
        let start_tool_count = 0; // Not used in new format

        let (result, _) =
            parse_caret_tool_invocations(text, request_id, start_tool_count, None).unwrap();
        assert_eq!(result.len(), 1);

        // Tool ID should follow the format: "tool-<request_id>-<tool_index_in_request>"
        // First tool in request gets index 1
        let expected_id = format!("tool-{}-{}", request_id, 1);
        assert_eq!(result[0].id, expected_id);
        assert_eq!(result[0].id, "tool-456-1");
    }

    #[tokio::test]
    async fn test_parse_caret_tool_invocations_multiple_tools_id_format() {
        let text = concat!(
            "^^^list_projects\n",
            "^^^\n",
            "^^^list_files\n",
            "project: test\n",
            "paths: [\n",
            "src\n",
            "]\n",
            "^^^\n",
            "^^^write_file\n",
            "project: test\n",
            "path: test.txt\n",
            "content: hello\n",
            "^^^"
        );

        let request_id = 789;
        let (result, _) = parse_caret_tool_invocations(text, request_id, 0, None).unwrap();
        assert_eq!(result.len(), 3);

        // First tool should get index 1
        assert_eq!(result[0].id, "tool-789-1");
        assert_eq!(result[0].name, "list_projects");

        // Second tool should get index 2
        assert_eq!(result[1].id, "tool-789-2");
        assert_eq!(result[1].name, "list_files");

        // Third tool should get index 3
        assert_eq!(result[2].id, "tool-789-3");
        assert_eq!(result[2].name, "write_file");
    }
}
