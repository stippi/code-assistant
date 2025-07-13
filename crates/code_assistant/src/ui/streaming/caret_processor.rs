//! Caret-style tool invocation processor for streaming responses
use crate::ui::streaming::{DisplayFragment, StreamProcessorTrait};
use crate::ui::{UIError, UserInterface};
use llm::{Message, MessageContent, StreamingChunk};
use regex::Regex;
use std::sync::Arc;

#[derive(Debug, PartialEq, Clone)]
enum ParserState {
    OutsideTool,
    InsideTool,
    CollectingMultiline {
        param_name: String,
        content: String,
        tool_id: String,
    },
    CollectingArray {
        param_name: String,
        elements: Vec<String>,
        tool_id: String,
    },
}

pub struct CaretStreamProcessor {
    ui: Arc<Box<dyn UserInterface>>,
    request_id: u64,
    buffer: String,
    state: ParserState,
    tool_regex: Regex,
    multiline_start_regex: Regex,
    multiline_end_regex: Regex,
    current_tool_id: String,
}

impl StreamProcessorTrait for CaretStreamProcessor {
    fn new(ui: Arc<Box<dyn UserInterface>>, request_id: u64) -> Self {
        Self {
            ui,
            request_id,
            buffer: String::new(),
            state: ParserState::OutsideTool,
            tool_regex: Regex::new(r"^\^\^\^([a-zA-Z0-9_]+)$").unwrap(),
            multiline_start_regex: Regex::new(r"^([a-zA-Z0-9_]+)\s+---$").unwrap(),
            multiline_end_regex: Regex::new(r"^---\s+([a-zA-Z0-9_]+)$").unwrap(),
            current_tool_id: String::new(),
        }
    }

    fn process(&mut self, chunk: &StreamingChunk) -> Result<(), UIError> {
        if let StreamingChunk::Text(text) = chunk {
            self.buffer.push_str(text);
        }
        self.process_buffer()?;
        Ok(())
    }

    fn extract_fragments_from_message(
        &mut self,
        message: &Message,
    ) -> Result<Vec<DisplayFragment>, UIError> {
        let text = if let MessageContent::Text(text) = &message.content {
            text
        } else {
            return Ok(vec![]);
        };

        let mut fragments = Vec::new();
        if let Some(plain_text) = text.split("^^^").next() {
            if !plain_text.is_empty() {
                fragments.push(DisplayFragment::PlainText(plain_text.to_string()));
            }
        }

        let tool_blocks = text.split("^^^").skip(1);
        for block in tool_blocks {
            if let Some(tool_name) = block.lines().next().map(|l| l.trim()) {
                if tool_name.is_empty() {
                    continue;
                }
                let tool_id = format!("{}_{}", tool_name, self.request_id);
                fragments.push(DisplayFragment::ToolName {
                    name: tool_name.to_string(),
                    id: tool_id.clone(),
                });

                let tool_content = block.lines().skip(1).collect::<Vec<_>>().join("\n");
                let params = self.parse_tool_parameters(&tool_content)?;

                for (name, value) in params {
                    fragments.push(DisplayFragment::ToolParameter {
                        name,
                        value,
                        tool_id: tool_id.clone(),
                    });
                }
                fragments.push(DisplayFragment::ToolEnd { id: tool_id });
            }
        }

        Ok(fragments)
    }
}

impl CaretStreamProcessor {
    fn process_buffer(&mut self) -> Result<(), UIError> {
        while let Some(pos) = self.buffer.find('\n') {
            let line = self.buffer.drain(..=pos).collect::<String>();
            self.process_line(line.trim_end())?;
        }
        Ok(())
    }

    fn process_line(&mut self, line: &str) -> Result<(), UIError> {
        let state = self.state.clone();
        match state {
            ParserState::OutsideTool => self.process_line_outside_tool(line),
            ParserState::InsideTool => self.process_line_inside_tool(line),
            ParserState::CollectingMultiline {
                param_name,
                mut content,
                tool_id,
            } => {
                if let Some(caps) = self.multiline_end_regex.captures(line) {
                    if caps.get(1).map_or(false, |m| m.as_str() == param_name) {
                        self.send_tool_parameter(&param_name, content.trim_end(), &tool_id)?;
                        self.state = ParserState::InsideTool;
                    } else {
                        content.push_str(line);
                        content.push('\n');
                        self.state = ParserState::CollectingMultiline {
                            param_name,
                            content,
                            tool_id,
                        };
                    }
                } else {
                    content.push_str(line);
                    content.push('\n');
                    self.state = ParserState::CollectingMultiline {
                        param_name,
                        content,
                        tool_id,
                    };
                }
                Ok(())
            }
            ParserState::CollectingArray {
                param_name,
                mut elements,
                tool_id,
            } => {
                if line.trim() == "]" {
                    let value = format!(
                        "[{}]",
                        elements
                            .iter()
                            .map(|e| format!("\"{}\"", e))
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                    self.send_tool_parameter(&param_name, &value, &tool_id)?;
                    self.state = ParserState::InsideTool;
                } else if !line.trim().is_empty() {
                    elements.push(line.trim().to_string());
                    self.state = ParserState::CollectingArray {
                        param_name,
                        elements,
                        tool_id,
                    };
                }
                Ok(())
            }
        }
    }

    fn process_line_outside_tool(&mut self, line: &str) -> Result<(), UIError> {
        if let Some(caps) = self.tool_regex.captures(line) {
            let tool_name = caps.get(1).unwrap().as_str();
            self.current_tool_id = format!("{}_{}", tool_name, self.request_id);
            self.send_tool_start(tool_name, &self.current_tool_id)?;
            self.state = ParserState::InsideTool;
        } else {
            self.send_plain_text(&format!("{}\n", line))?;
        }
        Ok(())
    }

    fn process_line_inside_tool(&mut self, line: &str) -> Result<(), UIError> {
        if line == "^^^" {
            self.send_tool_end(&self.current_tool_id)?;
            self.state = ParserState::OutsideTool;
        } else if let Some(caps) = self.multiline_start_regex.captures(line) {
            let param_name = caps.get(1).unwrap().as_str().to_string();
            self.state = ParserState::CollectingMultiline {
                param_name,
                content: String::new(),
                tool_id: self.current_tool_id.clone(),
            };
        } else if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim();
            if value == "[" {
                self.state = ParserState::CollectingArray {
                    param_name: key,
                    elements: vec![],
                    tool_id: self.current_tool_id.clone(),
                };
            } else {
                self.send_tool_parameter(&key, value, &self.current_tool_id)?;
            }
        }
        Ok(())
    }

    pub fn finalize_buffer(&mut self) -> Result<(), UIError> {
        if !self.buffer.is_empty() {
            let buffer_clone = self.buffer.clone();
            self.process_line(&buffer_clone)?;
            self.buffer.clear();
        }

        match self.state.clone() {
            ParserState::InsideTool => {
                self.send_tool_end(&self.current_tool_id)?;
            }
            ParserState::CollectingMultiline {
                param_name,
                content,
                tool_id,
            } => {
                self.send_tool_parameter(&param_name, content.trim_end(), &tool_id)?;
                self.send_tool_end(&tool_id)?;
            }
            ParserState::CollectingArray {
                param_name,
                elements,
                tool_id,
            } => {
                let value = format!(
                    "[{}]",
                    elements
                        .iter()
                        .map(|e| format!("\"{}\"", e))
                        .collect::<Vec<_>>()
                        .join(",")
                );
                self.send_tool_parameter(&param_name, &value, &tool_id)?;
                self.send_tool_end(&tool_id)?;
            }
            _ => {}
        }
        self.state = ParserState::OutsideTool;
        Ok(())
    }

    fn parse_tool_parameters(&self, content: &str) -> Result<Vec<(String, String)>, UIError> {
        let mut params = Vec::new();
        let mut lines = content.lines().peekable();
        while let Some(line) = lines.next() {
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_string();
                let value = value.trim();
                if value == "[" {
                    let mut elements = vec![];
                    while let Some(element_line) = lines.next() {
                        if element_line.trim() == "]" {
                            break;
                        }
                        elements.push(format!("\"{}\"", element_line.trim()));
                    }
                    params.push((key, format!("[{}]", elements.join(","))));
                } else {
                    params.push((key, value.to_string()));
                }
            } else if let Some(caps) = self.multiline_start_regex.captures(line) {
                let param_name = caps.get(1).unwrap().as_str();
                let mut content = String::new();
                while let Some(content_line) = lines.next() {
                    if let Some(end_caps) = self.multiline_end_regex.captures(content_line) {
                        if end_caps.get(1).map_or(false, |m| m.as_str() == param_name) {
                            break;
                        }
                    }
                    content.push_str(content_line);
                    content.push('\n');
                }
                params.push((param_name.to_string(), content.trim_end().to_string()));
            }
        }
        Ok(params)
    }

    fn send_plain_text(&self, text: &str) -> Result<(), UIError> {
        if !text.is_empty() {
            self.ui
                .display_fragment(&DisplayFragment::PlainText(text.to_string()))?;
        }
        Ok(())
    }

    fn send_tool_start(&self, name: &str, id: &str) -> Result<(), UIError> {
        self.ui.display_fragment(&DisplayFragment::ToolName {
            name: name.to_string(),
            id: id.to_string(),
        })?;
        Ok(())
    }

    fn send_tool_parameter(&self, name: &str, value: &str, tool_id: &str) -> Result<(), UIError> {
        self.ui.display_fragment(&DisplayFragment::ToolParameter {
            name: name.to_string(),
            value: value.to_string(),
            tool_id: tool_id.to_string(),
        })?;
        Ok(())
    }

    fn send_tool_end(&self, id: &str) -> Result<(), UIError> {
        self.ui
            .display_fragment(&DisplayFragment::ToolEnd { id: id.to_string() })?;
        Ok(())
    }
}
