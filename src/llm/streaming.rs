use crate::llm::types::*;
use crate::llm::StreamingCallback;
use anyhow::Result;
use reqwest::Response;
use std::str;

/// Trait for handling streaming responses from LLM providers
pub trait StreamResponseHandler: Clone {
    /// Process a chunk of data from the stream
    fn process_chunk(&mut self, chunk: &[u8], callback: &StreamingCallback) -> Result<bool>;

    /// Get the final accumulated response
    fn into_response(self) -> Result<LLMResponse>;
}

/// Common line buffer functionality for streaming implementations
#[derive(Clone)]
pub struct LineBuffer {
    buffer: String,
}

impl LineBuffer {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    /// Process a chunk of data, calling the provided function for each complete line
    pub fn process_chunk<F>(&mut self, chunk: &[u8], mut line_handler: F) -> Result<()>
    where
        F: FnMut(&str) -> Result<()>,
    {
        let chunk_str = str::from_utf8(chunk)?;

        for c in chunk_str.chars() {
            if c == '\n' {
                if !self.buffer.is_empty() {
                    line_handler(&self.buffer)?;
                    self.buffer.clear();
                }
            } else {
                self.buffer.push(c);
            }
        }

        Ok(())
    }

    /// Process any remaining data in the buffer
    pub fn flush(&mut self, callback: &StreamingCallback) -> Result<()> {
        if !self.buffer.is_empty() {
            callback(&self.buffer)?;
            self.buffer.clear();
        }
        Ok(())
    }
}

/// Helper for accumulating text content during streaming
#[derive(Clone)]
pub struct ContentAccumulator {
    text: String,
}

impl ContentAccumulator {
    pub fn new() -> Self {
        Self {
            text: String::new(),
        }
    }

    pub fn append(&mut self, text: &str) -> Result<()> {
        self.text.push_str(text);
        Ok(())
    }

    pub fn into_content_block(self) -> Option<ContentBlock> {
        if self.text.is_empty() {
            None
        } else {
            Some(ContentBlock::Text { text: self.text })
        }
    }
}

/// Helper for processing SSE data lines
pub fn process_sse_data<T, F>(line: &str, parser: F) -> Result<Option<T>>
where
    F: Fn(&str) -> Result<T>,
{
    if let Some(data) = line.strip_prefix("data: ") {
        if data == "[DONE]" {
            return Ok(None);
        }
        Ok(Some(parser(data)?))
    } else {
        Ok(None)
    }
}

/// Helper for streaming from a response
pub async fn stream_response<H: StreamResponseHandler>(
    response: &mut Response,
    handler: &mut H,
    callback: &StreamingCallback,
) -> Result<LLMResponse> {
    let mut done = false;

    while !done {
        if let Some(chunk) = response.chunk().await? {
            done = handler.process_chunk(&chunk, callback)?;
        } else {
            break;
        }
    }

    handler.clone().into_response()
}
