use anyhow::Result;
use llm::Message;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

use crate::session::SessionConfig;
use crate::tools::ToolRequest;
use crate::types::{PlanState, ToolSyntax};

// ============================================================================
// Session Branching Types
// ============================================================================

/// Unique identifier for a message node within a session
pub type NodeId = u64;

/// A path through the conversation tree (list of node IDs from root to leaf)
pub type ConversationPath = Vec<NodeId>;

/// A single message node in the conversation tree
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MessageNode {
    /// Unique ID within this session
    pub id: NodeId,

    /// The actual message content
    pub message: Message,

    /// Parent node ID (None for root/first message)
    pub parent_id: Option<NodeId>,

    /// Creation timestamp (for ordering siblings)
    pub created_at: SystemTime,

    /// Plan state snapshot (only set if plan changed in this message's response)
    /// Used for efficient plan reconstruction when switching branches
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_snapshot: Option<PlanState>,
}

/// Information about a branch point in the conversation (for UI)
#[derive(Debug, Clone, PartialEq)]
pub struct BranchInfo {
    /// Node ID where the branch occurs (the node that has multiple children)
    pub parent_node_id: Option<NodeId>,

    /// All sibling node IDs at this branch point (different continuations)
    pub sibling_ids: Vec<NodeId>,

    /// Index of the currently active sibling (0-based)
    pub active_index: usize,
}

/// Model configuration for a session
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionModelConfig {
    /// Display name of the model from models.json
    pub model_name: String,
    /// Legacy recording path persisted in older session files (ignored at runtime)
    #[serde(default, rename = "record_path", skip_serializing)]
    _legacy_record_path: Option<PathBuf>,
    /// Legacy context token limit persisted in older session files (ignored at runtime)
    #[serde(default, rename = "context_token_limit", skip_serializing)]
    _legacy_context_token_limit: Option<u32>,
}

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

    // ========================================================================
    // Branching: Tree-based message storage
    // ========================================================================
    /// All message nodes in the session (tree structure)
    /// Key: NodeId, Value: MessageNode
    #[serde(default)]
    pub message_nodes: BTreeMap<NodeId, MessageNode>,

    /// The currently active path through the tree
    /// This determines which messages are shown and sent to LLM
    #[serde(default)]
    pub active_path: ConversationPath,

    /// Counter for generating unique node IDs
    #[serde(default = "default_next_node_id")]
    pub next_node_id: NodeId,

    // ========================================================================
    // Legacy: Linear message list (for migration from old sessions)
    // ========================================================================
    /// Legacy linear message history - migrated to message_nodes on first load
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,

    /// Serialized tool execution results
    pub tool_executions: Vec<SerializedToolExecution>,
    /// Current session plan (for the active path)
    #[serde(default)]
    pub plan: PlanState,
    /// Persistent session configuration
    #[serde(default)]
    pub config: SessionConfig,
    /// Counter for generating unique request IDs within this session
    #[serde(default)]
    pub next_request_id: u64,
    /// Model configuration for this session
    #[serde(default)]
    pub model_config: Option<SessionModelConfig>,
    /// Legacy fields kept for backward compatibility with existing session files
    #[serde(rename = "init_path", default, skip_serializing_if = "Option::is_none")]
    legacy_init_path: Option<PathBuf>,
    #[serde(
        rename = "initial_project",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    legacy_initial_project: Option<String>,
    #[serde(
        rename = "tool_syntax",
        alias = "tool_mode",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    legacy_tool_syntax: Option<ToolSyntax>,

    #[serde(
        rename = "use_diff_blocks",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    legacy_use_diff_blocks: Option<bool>,
    /// Legacy working memory from old session files (ignored)
    #[serde(rename = "working_memory", default, skip_serializing)]
    _legacy_working_memory: serde_json::Value,
}

fn default_next_node_id() -> NodeId {
    1
}

impl ChatSession {
    /// Merge any legacy top-level fields into the nested SessionConfig.
    pub fn ensure_config(&mut self) -> Result<()> {
        if let Some(init_path) = self.legacy_init_path.take() {
            self.config.init_path = Some(init_path);
        }
        if let Some(initial_project) = self.legacy_initial_project.take() {
            if !initial_project.is_empty() {
                self.config.initial_project = initial_project;
            }
        }
        if let Some(tool_syntax) = self.legacy_tool_syntax.take() {
            self.config.tool_syntax = tool_syntax;
        }
        if let Some(use_diff_blocks) = self.legacy_use_diff_blocks.take() {
            self.config.use_diff_blocks = use_diff_blocks;
        }

        // Migrate linear messages to tree structure if needed
        self.migrate_to_tree_structure();

        Ok(())
    }

    /// Create a new empty chat session using the provided configuration.
    pub fn new_empty(
        id: String,
        name: String,
        config: SessionConfig,
        model_config: Option<SessionModelConfig>,
    ) -> Self {
        Self {
            id,
            name,
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            message_nodes: BTreeMap::new(),
            active_path: Vec::new(),
            next_node_id: 1,
            messages: Vec::new(),
            tool_executions: Vec::new(),
            plan: PlanState::default(),
            config,
            next_request_id: 1,
            model_config,
            legacy_init_path: None,
            legacy_initial_project: None,
            legacy_tool_syntax: None,
            legacy_use_diff_blocks: None,
            _legacy_working_memory: serde_json::Value::Null,
        }
    }

    // ========================================================================
    // Migration
    // ========================================================================

    /// Migrate legacy linear messages to tree structure.
    /// Called automatically by ensure_config() on session load.
    fn migrate_to_tree_structure(&mut self) {
        if self.message_nodes.is_empty() && !self.messages.is_empty() {
            debug!(
                "Migrating session {} from linear to tree structure ({} messages)",
                self.id,
                self.messages.len()
            );

            let mut parent_id: Option<NodeId> = None;

            for message in self.messages.drain(..) {
                let node_id = self.next_node_id;
                self.next_node_id += 1;

                let node = MessageNode {
                    id: node_id,
                    message,
                    parent_id,
                    created_at: SystemTime::now(),
                    plan_snapshot: None,
                };

                self.message_nodes.insert(node_id, node);
                self.active_path.push(node_id);
                parent_id = Some(node_id);
            }

            debug!(
                "Migration complete: {} nodes, active_path length: {}",
                self.message_nodes.len(),
                self.active_path.len()
            );
        }
    }

    // ========================================================================
    // Tree Navigation & Query
    // ========================================================================

    /// Get the linearized message history for the active path.
    /// This is what gets sent to the LLM.
    pub fn get_active_messages(&self) -> Vec<&Message> {
        self.active_path
            .iter()
            .filter_map(|id| self.message_nodes.get(id))
            .map(|node| &node.message)
            .collect()
    }

    /// Get owned copies of messages for the active path.
    pub fn get_active_messages_cloned(&self) -> Vec<Message> {
        self.active_path
            .iter()
            .filter_map(|id| self.message_nodes.get(id))
            .map(|node| node.message.clone())
            .collect()
    }

    /// Get all direct children of a node.
    pub fn get_children(&self, parent_id: Option<NodeId>) -> Vec<&MessageNode> {
        self.message_nodes
            .values()
            .filter(|node| node.parent_id == parent_id)
            .collect()
    }

    /// Get children sorted by creation time (oldest first).
    pub fn get_children_sorted(&self, parent_id: Option<NodeId>) -> Vec<&MessageNode> {
        let mut children = self.get_children(parent_id);
        children.sort_by_key(|n| n.created_at);
        children
    }

    /// Get branch info for a specific node (if it's part of a branch).
    /// Returns None if the node has no siblings (no branching at this point).
    pub fn get_branch_info(&self, node_id: NodeId) -> Option<BranchInfo> {
        let node = self.message_nodes.get(&node_id)?;
        let siblings: Vec<NodeId> = self
            .get_children_sorted(node.parent_id)
            .into_iter()
            .map(|n| n.id)
            .collect();

        if siblings.len() <= 1 {
            return None; // No branching here
        }

        let active_index = siblings.iter().position(|&id| id == node_id)?;

        Some(BranchInfo {
            parent_node_id: node.parent_id,
            sibling_ids: siblings,
            active_index,
        })
    }

    /// Find the plan state for the active path by walking backwards
    /// to find the most recent plan_snapshot.
    pub fn get_plan_for_active_path(&self) -> PlanState {
        for &node_id in self.active_path.iter().rev() {
            if let Some(node) = self.message_nodes.get(&node_id) {
                if let Some(plan) = &node.plan_snapshot {
                    return plan.clone();
                }
            }
        }
        PlanState::default()
    }

    // ========================================================================
    // Tree Modification
    // ========================================================================

    /// Add a new message as a child of the last node in the active path.
    /// Updates active_path to include the new node.
    /// Returns the new node ID.
    pub fn add_message(&mut self, message: Message) -> NodeId {
        self.add_message_with_parent(message, self.active_path.last().copied())
    }

    /// Add a new message as a child of a specific parent node.
    /// Updates active_path to follow this new branch.
    /// Returns the new node ID.
    pub fn add_message_with_parent(
        &mut self,
        message: Message,
        parent_id: Option<NodeId>,
    ) -> NodeId {
        let node_id = self.next_node_id;
        self.next_node_id += 1;

        let node = MessageNode {
            id: node_id,
            message,
            parent_id,
            created_at: SystemTime::now(),
            plan_snapshot: None,
        };

        self.message_nodes.insert(node_id, node);

        // Update active_path: build path to parent and add new node
        if let Some(parent) = parent_id {
            if let Some(parent_pos) = self.active_path.iter().position(|&id| id == parent) {
                // Parent is in current active path - just truncate
                self.active_path.truncate(parent_pos + 1);
            } else {
                // Parent is NOT in current active path (we're branching from a different branch)
                // Rebuild the path from root to parent
                self.active_path = self.build_path_to_node(parent);
            }
        } else {
            // No parent means this is a root node
            self.active_path.clear();
        }
        self.active_path.push(node_id);

        self.updated_at = SystemTime::now();
        node_id
    }

    /// Switch to a different branch by making a different sibling node active.
    /// Updates active_path to follow the new branch to its deepest descendant.
    pub fn switch_branch(&mut self, new_node_id: NodeId) -> Result<()> {
        let node = self
            .message_nodes
            .get(&new_node_id)
            .ok_or_else(|| anyhow::anyhow!("Node not found: {}", new_node_id))?;

        // Find where in active_path the parent is
        if let Some(parent_id) = node.parent_id {
            if let Some(parent_pos) = self.active_path.iter().position(|&id| id == parent_id) {
                // Truncate path after parent
                self.active_path.truncate(parent_pos + 1);
            } else {
                // Parent not in active path - this shouldn't happen in normal use
                // but we handle it by rebuilding the path from root
                self.active_path = self.build_path_to_node(parent_id);
            }
        } else {
            // Switching to a root node
            self.active_path.clear();
        }

        // Extend path from new node to deepest descendant
        self.extend_active_path_from(new_node_id);

        // Update the plan to match the new active path
        self.plan = self.get_plan_for_active_path();

        Ok(())
    }

    /// Build the path from root to a specific node.
    fn build_path_to_node(&self, target_id: NodeId) -> ConversationPath {
        let mut path = Vec::new();
        let mut current_id = Some(target_id);

        // Walk up to root, collecting node IDs
        while let Some(id) = current_id {
            path.push(id);
            current_id = self.message_nodes.get(&id).and_then(|n| n.parent_id);
        }

        // Reverse to get root-to-target order
        path.reverse();
        path
    }

    /// Extend active_path from a given node, following the most recent child at each step.
    fn extend_active_path_from(&mut self, start_node_id: NodeId) {
        self.active_path.push(start_node_id);

        let mut current_id = start_node_id;
        loop {
            // Collect child IDs to avoid borrowing issues
            let mut child_ids: Vec<(NodeId, SystemTime)> = self
                .message_nodes
                .values()
                .filter(|node| node.parent_id == Some(current_id))
                .map(|node| (node.id, node.created_at))
                .collect();

            if child_ids.is_empty() {
                break;
            }

            // Sort by creation time and take the most recent (last)
            child_ids.sort_by_key(|(_, created_at)| *created_at);
            let next_id = child_ids.last().unwrap().0;

            self.active_path.push(next_id);
            current_id = next_id;
        }
    }

    /// Get the total number of messages (nodes) in the session.
    pub fn message_count(&self) -> usize {
        self.message_nodes.len()
    }

    /// Check if the session has any branches.
    #[allow(dead_code)] // Used by tests, will be used by UI in Phase 4
    pub fn has_branches(&self) -> bool {
        // A session has branches if any node has more than one child
        let mut child_counts: HashMap<Option<NodeId>, usize> = HashMap::new();
        for node in self.message_nodes.values() {
            *child_counts.entry(node.parent_id).or_insert(0) += 1;
        }
        child_counts.values().any(|&count| count > 1)
    }
}

impl SessionModelConfig {
    /// Construct a session model configuration for the given display name.
    pub fn new(model_name: String) -> Self {
        Self {
            model_name,
            _legacy_record_path: None,
            _legacy_context_token_limit: None,
        }
    }

    #[cfg(test)]
    pub fn new_for_tests(model_name: String) -> Self {
        Self {
            model_name,
            _legacy_record_path: None,
            _legacy_context_token_limit: None,
        }
    }
}

/// A helper to obtain the tool syntax for this session without exposing legacy fields.
impl ChatSession {
    pub fn tool_syntax(&self) -> ToolSyntax {
        self.config.tool_syntax
    }

    pub fn initial_project(&self) -> &str {
        &self.config.initial_project
    }
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
        Ok(chats_dir.join(format!("{session_id}.json")))
    }

    fn metadata_file_path(&self) -> Result<PathBuf> {
        let chats_dir = self.ensure_chats_dir()?;
        Ok(chats_dir.join("metadata.json"))
    }

    pub fn save_chat_session(&mut self, session: &ChatSession) -> Result<()> {
        let mut session = session.clone();
        session.ensure_config()?;

        let session_path = self.chat_file_path(&session.id)?;
        debug!("Saving chat session to {}", session_path.display());
        let json = serde_json::to_string_pretty(&session)?;
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
            message_count: session.message_count(),
            total_usage,
            last_usage,
            tokens_limit,
            tool_syntax: session.tool_syntax(),
            initial_project: session.initial_project().to_string(),
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
        let mut session: ChatSession = serde_json::from_str(&json)?;
        session.ensure_config()?;
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

    /// Delete all empty sessions (sessions with no messages).
    /// Returns the number of deleted sessions.
    ///
    /// This method uses metadata to identify potentially empty sessions first,
    /// avoiding the need to load all session files. For safety, it verifies
    /// each candidate session is actually empty before deleting.
    pub fn delete_empty_sessions(&mut self) -> Result<usize> {
        let sessions_dir = self.root_dir.join("sessions");
        if !sessions_dir.exists() {
            return Ok(0);
        }

        // Use metadata to find candidate empty sessions (message_count == 0)
        let metadata_list = self.list_chat_sessions()?;
        let candidate_ids: Vec<String> = metadata_list
            .iter()
            .filter(|m| m.message_count == 0)
            .map(|m| m.id.clone())
            .collect();

        if candidate_ids.is_empty() {
            return Ok(0);
        }

        let mut deleted_count = 0;

        // Verify and delete each candidate
        for session_id in candidate_ids {
            // Safety check: load the session and verify it's actually empty
            match self.load_chat_session(&session_id) {
                Ok(Some(session)) if session.message_count() == 0 => {
                    if let Err(e) = self.delete_chat_session(&session_id) {
                        warn!("Failed to delete empty session {}: {}", session_id, e);
                    } else {
                        info!("Deleted empty session: {}", session_id);
                        deleted_count += 1;
                    }
                }
                Ok(Some(_)) => {
                    // Metadata was out of sync, session has messages - skip
                    debug!(
                        "Session {} has messages despite metadata saying 0, skipping",
                        session_id
                    );
                }
                Ok(None) => {
                    // Session file doesn't exist, clean up metadata
                    debug!(
                        "Session {} file not found, cleaning up metadata",
                        session_id
                    );
                    let _ = self.delete_chat_session(&session_id);
                }
                Err(e) => {
                    warn!(
                        "Failed to load session {} for verification: {}",
                        session_id, e
                    );
                }
            }
        }

        if deleted_count > 0 {
            info!("Cleaned up {} empty session(s)", deleted_count);
        }

        Ok(deleted_count)
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
                        session.id,
                        session.initial_project()
                    );

                    let metadata = ChatMetadata {
                        id: session.id.clone(),
                        name: session.name.clone(),
                        created_at: session.created_at,
                        updated_at: session.updated_at,
                        message_count: session.message_count(),
                        total_usage,
                        last_usage,
                        tokens_limit,
                        tool_syntax: session.tool_syntax(),
                        initial_project: session.initial_project().to_string(),
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

    format!("chat_{timestamp:x}_{random_part:x}")
}

/// Calculate usage information from session messages.
/// Uses tree structure (message_nodes) if available, falls back to legacy messages.
fn calculate_session_usage(session: &ChatSession) -> (llm::Usage, llm::Usage, Option<u32>) {
    let mut total_usage = llm::Usage::zero();
    let mut last_usage = llm::Usage::zero();
    let tokens_limit = None;

    // Get messages from tree structure (active path) or legacy list
    let messages: Vec<&Message> = if !session.message_nodes.is_empty() {
        session.get_active_messages()
    } else {
        session.messages.iter().collect()
    };

    // Calculate total usage and find most recent assistant message usage
    for message in messages {
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
    /// Per-session mutexes to prevent concurrent writes to the same draft file
    session_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
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

        Ok(Self {
            drafts_dir,
            session_locks: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Get the path for a draft file for a given session
    fn draft_file_path(&self, session_id: &str) -> PathBuf {
        self.drafts_dir.join(format!("{session_id}.json"))
    }

    /// Get or create a mutex for the given session to prevent concurrent writes
    fn get_session_lock(&self, session_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.session_locks.lock().unwrap();
        locks
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Save a draft with attachments for a session
    pub fn save_draft(
        &self,
        session_id: &str,
        text_content: &str,
        attachments: &[DraftAttachment],
    ) -> Result<()> {
        // Acquire session-specific lock to prevent concurrent writes
        let session_lock = self.get_session_lock(session_id);
        let _guard = session_lock.lock().unwrap();

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
            .load_draft_struct_unlocked(session_id)?
            .unwrap_or_else(|| SessionDraft::new(session_id.to_string()));

        // Update message content and attachments
        draft.set_message(text_content.to_string());
        draft.attachments = attachments.to_vec();

        // Serialize and save
        let draft_json = serde_json::to_string_pretty(&draft)?;
        std::fs::write(&file_path, draft_json)?;

        Ok(())
    }

    /// Load a draft with attachments for a session
    pub fn load_draft(&self, session_id: &str) -> Result<Option<(String, Vec<DraftAttachment>)>> {
        let draft = self.load_draft_struct(session_id)?;
        Ok(draft.map(|d| (d.get_message(), d.attachments)))
    }

    /// Load the complete draft structure for a session
    pub fn load_draft_struct(&self, session_id: &str) -> Result<Option<SessionDraft>> {
        // Acquire session-specific lock to prevent reading during writes
        let session_lock = self.get_session_lock(session_id);
        let _guard = session_lock.lock().unwrap();

        self.load_draft_struct_unlocked(session_id)
    }

    /// Load the complete draft structure for a session without acquiring lock
    /// (for internal use when lock is already held)
    fn load_draft_struct_unlocked(&self, session_id: &str) -> Result<Option<SessionDraft>> {
        let file_path = self.draft_file_path(session_id);

        if !file_path.exists() {
            return Ok(None);
        }

        let json_content = std::fs::read_to_string(&file_path)?;
        let draft: SessionDraft = serde_json::from_str(&json_content)?;

        Ok(Some(draft))
    }

    /// Clear a draft for a session (used when message is sent)
    pub fn clear_draft(&self, session_id: &str) -> Result<()> {
        // Acquire session-specific lock to prevent concurrent operations
        let session_lock = self.get_session_lock(session_id);
        let _guard = session_lock.lock().unwrap();

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionConfig;
    use crate::types::{PlanItem, PlanItemPriority, PlanItemStatus};
    use tempfile::tempdir;

    #[test]
    fn chat_session_plan_roundtrip() {
        let mut session = ChatSession::new_empty(
            "session123".to_string(),
            "Test Session".to_string(),
            SessionConfig::default(),
            None,
        );

        session.plan.entries.push(PlanItem {
            content: "Review requirements".to_string(),
            priority: PlanItemPriority::High,
            status: PlanItemStatus::InProgress,
            meta: None,
        });
        session.plan.meta = Some(serde_json::json!({ "source": "unit-test" }));

        let serialized = serde_json::to_string(&session).expect("serialize");
        let restored: ChatSession = serde_json::from_str(&serialized).expect("deserialize");

        assert_eq!(restored.plan.entries.len(), 1);
        let entry = &restored.plan.entries[0];
        assert_eq!(entry.content, "Review requirements");
        assert_eq!(entry.priority, PlanItemPriority::High);
        assert_eq!(entry.status, PlanItemStatus::InProgress);

        let meta = restored.plan.meta.expect("plan meta should exist");
        assert_eq!(meta["source"], "unit-test");
    }

    #[test]
    fn delete_empty_sessions_removes_only_empty() {
        // Create a temporary directory for this test
        let temp_dir = tempdir().expect("failed to create temp dir");

        // Create a persistence instance using the temp directory
        let mut persistence = FileSessionPersistence {
            root_dir: temp_dir.path().to_path_buf(),
        };

        // Create an empty session
        let empty_session = ChatSession::new_empty(
            "empty_session".to_string(),
            "Empty Session".to_string(),
            SessionConfig::default(),
            None,
        );
        persistence
            .save_chat_session(&empty_session)
            .expect("save empty session");

        // Create a session with messages
        let mut non_empty_session = ChatSession::new_empty(
            "non_empty_session".to_string(),
            "Non-Empty Session".to_string(),
            SessionConfig::default(),
            None,
        );
        non_empty_session.messages.push(Message::new_user("Hello"));
        persistence
            .save_chat_session(&non_empty_session)
            .expect("save non-empty session");

        // Verify both sessions exist
        let sessions = persistence.list_chat_sessions().expect("list sessions");
        assert_eq!(sessions.len(), 2);

        // Delete empty sessions
        let deleted_count = persistence
            .delete_empty_sessions()
            .expect("delete empty sessions");

        // Should have deleted exactly one session
        assert_eq!(deleted_count, 1);

        // Verify only the non-empty session remains
        let remaining_sessions = persistence.list_chat_sessions().expect("list sessions");
        assert_eq!(remaining_sessions.len(), 1);
        assert_eq!(remaining_sessions[0].id, "non_empty_session");

        // Verify the empty session file is gone
        assert!(persistence
            .load_chat_session("empty_session")
            .expect("load")
            .is_none());

        // Verify the non-empty session still exists
        assert!(persistence
            .load_chat_session("non_empty_session")
            .expect("load")
            .is_some());
    }

    #[test]
    fn delete_empty_sessions_handles_no_sessions() {
        // Create a temporary directory for this test
        let temp_dir = tempdir().expect("failed to create temp dir");

        // Create a persistence instance using the temp directory
        let mut persistence = FileSessionPersistence {
            root_dir: temp_dir.path().to_path_buf(),
        };

        // Delete empty sessions when there are no sessions
        let deleted_count = persistence
            .delete_empty_sessions()
            .expect("delete empty sessions");

        assert_eq!(deleted_count, 0);
    }

    // ========================================================================
    // Session Branching Tests
    // ========================================================================

    #[test]
    fn test_add_message_creates_tree_structure() {
        let mut session = ChatSession::new_empty(
            "test".to_string(),
            "Test".to_string(),
            SessionConfig::default(),
            None,
        );

        // Add first message
        let node1 = session.add_message(Message::new_user("Hello"));
        assert_eq!(node1, 1);
        assert_eq!(session.active_path, vec![1]);
        assert_eq!(session.message_nodes.len(), 1);

        // Add second message
        let node2 = session.add_message(Message::new_assistant("Hi there!"));
        assert_eq!(node2, 2);
        assert_eq!(session.active_path, vec![1, 2]);
        assert_eq!(session.message_nodes.len(), 2);

        // Verify parent relationships
        assert_eq!(session.message_nodes.get(&1).unwrap().parent_id, None);
        assert_eq!(session.message_nodes.get(&2).unwrap().parent_id, Some(1));
    }

    #[test]
    fn test_add_message_with_parent_creates_branch() {
        let mut session = ChatSession::new_empty(
            "test".to_string(),
            "Test".to_string(),
            SessionConfig::default(),
            None,
        );

        // Create initial conversation
        let _node1 = session.add_message(Message::new_user("Hello"));
        let _node2 = session.add_message(Message::new_assistant("Hi!"));
        let _node3 = session.add_message(Message::new_user("How are you?"));

        // Now create a branch from node 2 (after the first assistant response)
        let branch_node = session.add_message_with_parent(
            Message::new_user("What's the weather like?"),
            Some(2), // Branch from node 2
        );

        assert_eq!(branch_node, 4);
        // Active path should now follow the new branch
        assert_eq!(session.active_path, vec![1, 2, 4]);

        // Both node 3 and node 4 should have parent_id = 2
        assert_eq!(session.message_nodes.get(&3).unwrap().parent_id, Some(2));
        assert_eq!(session.message_nodes.get(&4).unwrap().parent_id, Some(2));

        // Session should detect branches
        assert!(session.has_branches());
    }

    #[test]
    fn test_switch_branch() {
        let mut session = ChatSession::new_empty(
            "test".to_string(),
            "Test".to_string(),
            SessionConfig::default(),
            None,
        );

        // Create initial conversation
        session.add_message(Message::new_user("Hello")); // node 1
        session.add_message(Message::new_assistant("Hi!")); // node 2
        session.add_message(Message::new_user("Original followup")); // node 3

        // Create a branch from node 2
        session.add_message_with_parent(Message::new_user("Alternative followup"), Some(2)); // node 4

        // Add continuation on the branch
        session.add_message(Message::new_assistant("Alternative response")); // node 5

        // Active path should be: 1 -> 2 -> 4 -> 5
        assert_eq!(session.active_path, vec![1, 2, 4, 5]);

        // Switch back to node 3 (the original branch)
        session.switch_branch(3).expect("switch branch");

        // Active path should now be: 1 -> 2 -> 3
        assert_eq!(session.active_path, vec![1, 2, 3]);

        // Verify linearized messages
        let messages = session.get_active_messages();
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn test_get_branch_info() {
        let mut session = ChatSession::new_empty(
            "test".to_string(),
            "Test".to_string(),
            SessionConfig::default(),
            None,
        );

        // Create initial conversation
        session.add_message(Message::new_user("Hello")); // node 1
        session.add_message(Message::new_assistant("Hi!")); // node 2
        session.add_message(Message::new_user("Followup A")); // node 3

        // No branch info for node 3 yet (only child of node 2)
        assert!(session.get_branch_info(3).is_none());

        // Create branches from node 2
        session.add_message_with_parent(Message::new_user("Followup B"), Some(2)); // node 4
        session.add_message_with_parent(Message::new_user("Followup C"), Some(2)); // node 5

        // Now node 3, 4, 5 are siblings
        let info_3 = session.get_branch_info(3).expect("should have branch info");
        assert_eq!(info_3.parent_node_id, Some(2));
        assert_eq!(info_3.sibling_ids.len(), 3);

        let info_5 = session.get_branch_info(5).expect("should have branch info");
        assert_eq!(info_5.parent_node_id, Some(2));
        assert_eq!(info_5.active_index, 2); // node 5 is the third sibling
    }

    #[test]
    fn test_migration_from_linear_messages() {
        // Create a session with legacy linear messages
        let mut session = ChatSession {
            id: "test".to_string(),
            name: "Test".to_string(),
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            message_nodes: BTreeMap::new(),
            active_path: Vec::new(),
            next_node_id: 1,
            messages: vec![
                Message::new_user("Hello"),
                Message::new_assistant("Hi!"),
                Message::new_user("How are you?"),
            ],
            tool_executions: Vec::new(),
            plan: PlanState::default(),
            config: SessionConfig::default(),
            next_request_id: 1,
            model_config: None,
            legacy_init_path: None,
            legacy_initial_project: None,
            legacy_tool_syntax: None,
            legacy_use_diff_blocks: None,
            _legacy_working_memory: serde_json::Value::Null,
        };

        // Run migration
        session.ensure_config().expect("migration should succeed");

        // Verify migration
        assert_eq!(session.message_nodes.len(), 3);
        assert_eq!(session.active_path, vec![1, 2, 3]);
        assert!(session.messages.is_empty()); // Legacy messages should be cleared

        // Verify tree structure
        assert_eq!(session.message_nodes.get(&1).unwrap().parent_id, None);
        assert_eq!(session.message_nodes.get(&2).unwrap().parent_id, Some(1));
        assert_eq!(session.message_nodes.get(&3).unwrap().parent_id, Some(2));

        // Verify messages are accessible
        let messages = session.get_active_messages();
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn test_get_active_messages_cloned() {
        let mut session = ChatSession::new_empty(
            "test".to_string(),
            "Test".to_string(),
            SessionConfig::default(),
            None,
        );

        session.add_message(Message::new_user("Hello"));
        session.add_message(Message::new_assistant("Hi!"));

        let messages = session.get_active_messages_cloned();
        assert_eq!(messages.len(), 2);

        // Verify content
        match &messages[0].content {
            llm::MessageContent::Text(text) => assert_eq!(text, "Hello"),
            _ => panic!("Expected text content"),
        }
    }

    #[test]
    fn test_nested_branching() {
        let mut session = ChatSession::new_empty(
            "test".to_string(),
            "Test".to_string(),
            SessionConfig::default(),
            None,
        );

        // Level 1: Initial message
        session.add_message(Message::new_user("Start")); // 1

        // Level 2: Two branches from node 1
        session.add_message(Message::new_assistant("Response A")); // 2
        session.add_message_with_parent(Message::new_assistant("Response B"), Some(1)); // 3

        // Level 3: Two branches from node 2
        session.switch_branch(2).unwrap();
        session.add_message(Message::new_user("Follow A1")); // 4
        session.add_message_with_parent(Message::new_user("Follow A2"), Some(2)); // 5

        // Verify structure
        assert_eq!(session.message_nodes.len(), 5);

        // Check node 1 has two children (2 and 3)
        let children_of_1 = session.get_children(Some(1));
        assert_eq!(children_of_1.len(), 2);

        // Check node 2 has two children (4 and 5)
        let children_of_2 = session.get_children(Some(2));
        assert_eq!(children_of_2.len(), 2);

        // Navigate to different paths and verify
        session.switch_branch(3).unwrap();
        assert_eq!(session.active_path, vec![1, 3]);

        session.switch_branch(5).unwrap();
        assert_eq!(session.active_path, vec![1, 2, 5]);
    }

    #[test]
    fn test_branch_from_different_branch() {
        // This tests the scenario where we create a branch while on a different branch
        // i.e., the parent_id is NOT in the current active_path
        let mut session = ChatSession::new_empty(
            "test".to_string(),
            "Test".to_string(),
            SessionConfig::default(),
            None,
        );

        // Create initial conversation on main branch
        session.add_message(Message::new_user("User 1")); // node 1
        session.add_message(Message::new_assistant("Asst 1")); // node 2
        session.add_message(Message::new_user("User 2")); // node 3
        session.add_message(Message::new_assistant("Asst 2")); // node 4

        // active_path: [1, 2, 3, 4]
        assert_eq!(session.active_path, vec![1, 2, 3, 4]);

        // Create branch 2 from node 2 (alternative User 2)
        session.add_message_with_parent(Message::new_user("User 2 alt"), Some(2)); // node 5
        session.add_message(Message::new_assistant("Asst 2 alt")); // node 6

        // active_path: [1, 2, 5, 6] (we're now on branch 2)
        assert_eq!(session.active_path, vec![1, 2, 5, 6]);

        // Now while on branch 2, create a new branch from node 4 (which is on branch 1)
        // This should properly switch to branch 1's path and then add the new node
        let new_node =
            session.add_message_with_parent(Message::new_user("User 3 on branch 1"), Some(4)); // node 7

        assert_eq!(new_node, 7);
        // active_path should be: [1, 2, 3, 4, 7] - NOT [1, 2, 5, 6, 7]
        assert_eq!(session.active_path, vec![1, 2, 3, 4, 7]);

        // Verify parent relationship
        assert_eq!(session.message_nodes.get(&7).unwrap().parent_id, Some(4));

        // Verify we can still switch back to branch 2
        session.switch_branch(6).unwrap();
        assert_eq!(session.active_path, vec![1, 2, 5, 6]);
    }
}
