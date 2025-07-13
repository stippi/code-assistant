use anyhow::Result;
use llm::Message;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info};

use crate::tools::ToolRequest;
use crate::types::{ToolSyntax, WorkingMemory};

/// A complete chat session with all its data
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatSession {
    /// Unique identifier for the chat session
    pub id: String,
    /// User-friendly name for the chat
    pub name: String,
    /// Creation timestamp
    pub created_at: SystemTime,
    /// Last updated timestamp
    pub updated_at: SystemTime,
    /// Message history
    pub messages: Vec<Message>,
    /// Serialized tool execution results
    pub tool_executions: Vec<SerializedToolExecution>,
    /// Working memory state
    pub working_memory: WorkingMemory,
    /// Initial project path (if any)
    pub init_path: Option<PathBuf>,
    /// Initial project name
    pub initial_project: Option<String>,
    /// Tool syntax used for this session (XML or Native)
    pub tool_syntax: ToolSyntax,
    /// Counter for generating unique request IDs within this session
    #[serde(default)]
    pub next_request_id: u64,
}

/// Serialized representation of a tool execution
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SerializedToolExecution {
    /// Tool request details
    pub tool_request: ToolRequest,
    /// Serialized tool result as JSON
    pub result_json: serde_json::Value,
    /// Tool name for deserialization
    pub tool_name: String,
}

/// Metadata for a chat session (used for listing)
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ChatMetadata {
    pub id: String,
    pub name: String,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    pub message_count: usize,
    /// Total usage across the entire session
    #[serde(default)]
    pub total_usage: llm::Usage,
    /// Usage from the last assistant message
    #[serde(default)]
    pub last_usage: llm::Usage,
    /// Token limit from rate limiting headers (if available)
    #[serde(default)]
    pub tokens_limit: Option<u32>,
}

#[derive(Clone)]
pub struct FileSessionPersistence {
    root_dir: PathBuf,
}

impl FileSessionPersistence {
    pub fn new() -> Self {
        let root_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("code-assistant");
        info!("Storing sessions in: {:?}", root_dir.to_path_buf());
        Self { root_dir }
    }

    fn ensure_chats_dir(&self) -> Result<PathBuf> {
        let chats_dir = self.root_dir.join("sessions");
        if !chats_dir.exists() {
            std::fs::create_dir_all(&chats_dir)?;
        }
        Ok(chats_dir)
    }

    fn chat_file_path(&self, session_id: &str) -> Result<PathBuf> {
        let chats_dir = self.ensure_chats_dir()?;
        Ok(chats_dir.join(format!("{}.json", session_id)))
    }

    fn metadata_file_path(&self) -> Result<PathBuf> {
        let chats_dir = self.ensure_chats_dir()?;
        Ok(chats_dir.join("metadata.json"))
    }

    pub fn save_chat_session(&mut self, session: &ChatSession) -> Result<()> {
        let session_path = self.chat_file_path(&session.id)?;
        debug!("Saving chat session to {}", session_path.display());
        let json = serde_json::to_string_pretty(session)?;
        std::fs::write(session_path, json)?;

        // Update metadata
        let metadata_path = self.metadata_file_path()?;
        let mut metadata_list: Vec<ChatMetadata> = if metadata_path.exists() {
            let content = std::fs::read_to_string(&metadata_path)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Vec::new()
        };

        // Calculate usage information
        let (total_usage, last_usage, tokens_limit) = calculate_session_usage(&session);

        // Update or add metadata for this session
        let new_metadata = ChatMetadata {
            id: session.id.clone(),
            name: session.name.clone(),
            created_at: session.created_at,
            updated_at: session.updated_at,
            message_count: session.messages.len(),
            total_usage,
            last_usage,
            tokens_limit,
        };

        if let Some(existing) = metadata_list.iter_mut().find(|m| m.id == session.id) {
            *existing = new_metadata;
        } else {
            metadata_list.push(new_metadata);
        }

        let metadata_json = serde_json::to_string_pretty(&metadata_list)?;
        std::fs::write(metadata_path, metadata_json)?;

        Ok(())
    }

    pub fn load_chat_session(&self, session_id: &str) -> Result<Option<ChatSession>> {
        let session_path = self.chat_file_path(session_id)?;
        if !session_path.exists() {
            return Ok(None);
        }

        debug!("Loading chat session from {}", session_path.display());
        let json = std::fs::read_to_string(session_path)?;
        let session = serde_json::from_str(&json)?;
        Ok(Some(session))
    }

    pub fn list_chat_sessions(&self) -> Result<Vec<ChatMetadata>> {
        let metadata_path = self.metadata_file_path()?;
        if !metadata_path.exists() {
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(metadata_path)?;
        let mut metadata_list: Vec<ChatMetadata> =
            serde_json::from_str(&content).unwrap_or_default();

        // Sort by updated_at in descending order (newest first)
        metadata_list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        Ok(metadata_list)
    }

    #[allow(dead_code)]
    pub fn get_chat_session_metadata(&self, session_id: &str) -> Result<Option<ChatMetadata>> {
        let metadata_path = self.metadata_file_path()?;
        if !metadata_path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(metadata_path)?;
        let metadata_list: Vec<ChatMetadata> = serde_json::from_str(&content).unwrap_or_default();

        Ok(metadata_list.into_iter().find(|m| m.id == session_id))
    }

    pub fn delete_chat_session(&mut self, session_id: &str) -> Result<()> {
        // Remove the session file
        let session_path = self.chat_file_path(session_id)?;
        if session_path.exists() {
            debug!("Deleting chat session file {}", session_path.display());
            std::fs::remove_file(session_path)?;
        }

        // Update metadata to remove this session
        let metadata_path = self.metadata_file_path()?;
        if metadata_path.exists() {
            let content = std::fs::read_to_string(&metadata_path)?;
            let mut metadata_list: Vec<ChatMetadata> =
                serde_json::from_str(&content).unwrap_or_default();

            metadata_list.retain(|m| m.id != session_id);

            let metadata_json = serde_json::to_string_pretty(&metadata_list)?;
            std::fs::write(metadata_path, metadata_json)?;
        }

        Ok(())
    }

    pub fn get_latest_session_id(&self) -> Result<Option<String>> {
        let sessions = self.list_chat_sessions()?;
        Ok(sessions.first().map(|s| s.id.clone()))
    }
}

/// Generate a unique session ID
pub fn generate_session_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Simple random component using timestamp
    let random_part = (timestamp % 10000) + (std::process::id() as u64 % 1000);

    format!("chat_{:x}_{:x}", timestamp, random_part)
}

/// Calculate usage information from session messages
fn calculate_session_usage(session: &ChatSession) -> (llm::Usage, llm::Usage, Option<u32>) {
    let mut total_usage = llm::Usage::zero();
    let mut last_usage = llm::Usage::zero();
    let tokens_limit = None;

    // Calculate total usage and find most recent assistant message usage
    for message in &session.messages {
        if let Some(usage) = &message.usage {
            // Add to total usage
            total_usage.input_tokens += usage.input_tokens;
            total_usage.output_tokens += usage.output_tokens;
            total_usage.cache_creation_input_tokens += usage.cache_creation_input_tokens;
            total_usage.cache_read_input_tokens += usage.cache_read_input_tokens;

            // For assistant messages, update last usage (most recent wins)
            if matches!(message.role, llm::MessageRole::Assistant) {
                last_usage = usage.clone();
            }
        }
    }

    // Note: We don't have access to rate_limit_info in persisted messages currently
    // This could be added later if needed, but tokens_limit is usually constant per provider

    (total_usage, last_usage, tokens_limit)
}
