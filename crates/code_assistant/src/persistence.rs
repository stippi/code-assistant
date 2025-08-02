use anyhow::Result;
use llm::Message;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

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
    pub initial_project: String,
    /// Tool syntax used for this session (XML or Native)
    #[serde(alias = "tool_mode")]
    pub tool_syntax: ToolSyntax,
    /// Whether this session uses diff blocks format (replace_in_file vs edit tool)
    #[serde(default)]
    pub use_diff_blocks: bool,
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
    /// Tool syntax used for this session
    pub tool_syntax: ToolSyntax,
    /// Initial project name
    pub initial_project: String,
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
            tool_syntax: session.tool_syntax,
            initial_project: session.initial_project.clone(),
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
            match serde_json::from_str::<Vec<ChatMetadata>>(&content) {
                Ok(list) => {
                    debug!(
                        "Successfully parsed metadata file with {} entries",
                        list.len()
                    );
                    list
                }
                Err(e) => {
                    warn!(
                        "Failed to deserialize chat metadata, will rebuild from sessions: {}",
                        e
                    );
                    debug!("Metadata content that failed to parse: {}", content);
                    // Try to rebuild metadata from existing session files
                    self.rebuild_metadata_from_sessions()?
                }
            };

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

    /// Rebuild metadata from existing session files (used when metadata file is corrupted)
    fn rebuild_metadata_from_sessions(&self) -> Result<Vec<ChatMetadata>> {
        let mut metadata_list = Vec::new();

        // Get all session files
        let sessions_dir = self.root_dir.join("sessions");
        if !sessions_dir.exists() {
            return Ok(metadata_list);
        }

        for entry in std::fs::read_dir(sessions_dir)? {
            let entry = entry?;
            let path = entry.path();

            // Only process .json files
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            // Extract session ID from filename
            if let Some(filename) = path.file_stem().and_then(|s| s.to_str()) {
                if let Ok(Some(session)) = self.load_chat_session(filename) {
                    // Calculate usage information
                    let (total_usage, last_usage, tokens_limit) = calculate_session_usage(&session);

                    debug!(
                        "Rebuilding metadata for session {}: initial_project='{}'",
                        session.id, session.initial_project
                    );

                    let metadata = ChatMetadata {
                        id: session.id.clone(),
                        name: session.name.clone(),
                        created_at: session.created_at,
                        updated_at: session.updated_at,
                        message_count: session.messages.len(),
                        total_usage,
                        last_usage,
                        tokens_limit,
                        tool_syntax: session.tool_syntax,
                        initial_project: session.initial_project.clone(),
                    };

                    metadata_list.push(metadata);
                }
            }
        }

        // Save the rebuilt metadata
        if !metadata_list.is_empty() {
            if let Err(e) = self.save_metadata_list(&metadata_list) {
                warn!("Failed to save rebuilt metadata: {}", e);
            } else {
                info!(
                    "Successfully rebuilt metadata for {} sessions",
                    metadata_list.len()
                );
            }
        }

        Ok(metadata_list)
    }

    /// Helper method to save metadata list to file
    fn save_metadata_list(&self, metadata_list: &[ChatMetadata]) -> Result<()> {
        let metadata_path = self.metadata_file_path()?;
        let metadata_json = serde_json::to_string_pretty(metadata_list)?;
        std::fs::write(metadata_path, metadata_json)?;
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

/// Draft attachment types for extensibility
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DraftAttachment {
    #[serde(rename = "text")]
    Text { content: String },
    #[serde(rename = "image")]
    Image { content: String, mime_type: String }, // Base64 encoded
    #[serde(rename = "file")]
    File {
        content: String,
        filename: String,
        mime_type: String,
    },
}

/// Complete draft structure for a session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDraft {
    pub session_id: String,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// The main message text that the user types
    pub message: String,
    /// Additional attachments (images, files, etc.)
    pub attachments: Vec<DraftAttachment>,
}

impl SessionDraft {
    pub fn new(session_id: String) -> Self {
        let now = SystemTime::now();
        Self {
            session_id,
            created_at: now,
            updated_at: now,
            message: String::new(),
            attachments: Vec::new(),
        }
    }

    pub fn set_message(&mut self, message: String) {
        self.updated_at = SystemTime::now();
        self.message = message;
    }

    pub fn get_message(&self) -> String {
        self.message.clone()
    }
}

/// Storage for draft messages per session
#[derive(Debug, Clone)]
pub struct DraftStorage {
    drafts_dir: PathBuf,
}

impl DraftStorage {
    /// Create a new DraftStorage instance
    pub fn new(base_dir: PathBuf) -> Result<Self> {
        let drafts_dir = base_dir.join("drafts");

        // Create drafts directory if it doesn't exist
        if !drafts_dir.exists() {
            std::fs::create_dir_all(&drafts_dir)?;
            debug!("Created drafts directory: {}", drafts_dir.display());
        }

        Ok(Self { drafts_dir })
    }

    /// Get the path for a draft file for a given session
    fn draft_file_path(&self, session_id: &str) -> PathBuf {
        self.drafts_dir.join(format!("{}.json", session_id))
    }

    /// Save a draft with attachments for a session
    pub fn save_draft(
        &self,
        session_id: &str,
        text_content: &str,
        attachments: &[DraftAttachment],
    ) -> Result<()> {
        let file_path = self.draft_file_path(session_id);

        if text_content.is_empty() && attachments.is_empty() {
            // Remove the draft file if it exists
            if file_path.exists() {
                std::fs::remove_file(&file_path)?;
                debug!("Cleared empty draft for session: {}", session_id);
            }
            return Ok(());
        }

        // Load existing draft or create new one
        let mut draft = self
            .load_draft_struct(session_id)?
            .unwrap_or_else(|| SessionDraft::new(session_id.to_string()));

        // Update message content and attachments
        draft.set_message(text_content.to_string());
        draft.attachments = attachments.to_vec();

        // Serialize and save
        let draft_json = serde_json::to_string_pretty(&draft)?;
        std::fs::write(&file_path, draft_json)?;

        debug!(
            "Saved draft with {} attachments for session: {}",
            attachments.len(),
            session_id
        );
        Ok(())
    }

    /// Load a draft with attachments for a session
    pub fn load_draft(&self, session_id: &str) -> Result<Option<(String, Vec<DraftAttachment>)>> {
        let draft = self.load_draft_struct(session_id)?;
        Ok(draft.map(|d| (d.get_message(), d.attachments)))
    }

    /// Load the complete draft structure for a session
    pub fn load_draft_struct(&self, session_id: &str) -> Result<Option<SessionDraft>> {
        let file_path = self.draft_file_path(session_id);

        if !file_path.exists() {
            return Ok(None);
        }

        let json_content = std::fs::read_to_string(&file_path)?;
        let draft: SessionDraft = serde_json::from_str(&json_content)?;

        let message = draft.get_message();
        debug!(
            "Loaded draft for session {}: {} characters",
            session_id,
            message.len()
        );
        Ok(Some(draft))
    }

    /// Clear a draft for a session (used when message is sent)
    pub fn clear_draft(&self, session_id: &str) -> Result<()> {
        let file_path = self.draft_file_path(session_id);

        if file_path.exists() {
            std::fs::remove_file(&file_path)?;
            debug!("Cleared draft for session: {}", session_id);
        }

        Ok(())
    }

    /// Clean up old drafts for sessions that no longer exist
    #[allow(dead_code)]
    pub fn cleanup_orphaned_drafts(&self, existing_session_ids: &[String]) -> Result<()> {
        if !self.drafts_dir.exists() {
            return Ok(());
        }

        let mut cleaned_count = 0;
        for entry in std::fs::read_dir(&self.drafts_dir)? {
            let entry = entry?;
            let path = entry.path();

            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(session_id) = file_name.strip_suffix(".json") {
                    if !existing_session_ids.contains(&session_id.to_string()) {
                        std::fs::remove_file(&path)?;
                        cleaned_count += 1;
                        debug!("Cleaned up orphaned draft: {}", session_id);
                    }
                }
            }
        }

        if cleaned_count > 0 {
            info!("Cleaned up {} orphaned draft files", cleaned_count);
        }

        Ok(())
    }
}
