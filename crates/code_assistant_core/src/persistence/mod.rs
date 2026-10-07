use anyhow::Result;
use llm::Message;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::SystemTime;
use tracing::{debug, info, warn};

use crate::session::SessionConfig;
use crate::session::lifecycle::SessionLifecycle;
use crate::types::{PlanState, ToolSyntax};
use crate::utils::file_utils::{FileLockGuard, atomic_write_json, lock_exclusive};

mod blobs;
mod journal;
pub mod layout;
pub use layout::{SessionLayout, SessionPath};

// ============================================================================
// Session Branching Types
// ============================================================================

// The conversation tree types live in the agent core; re-exported here for
// use alongside the session persistence types.
pub use agent_core::{ConversationPath, MessageNode, NodeId};

/// Cap oversized images carried by a freshly loaded session's conversation so
/// no edge exceeds `max_edge`. Walks both the message-node tree (authoritative)
/// and any legacy linear messages, correcting `ContentBlock::Image` blocks in
/// place. Tool-result images live in `tool_executions` and are corrected when
/// those records are deserialized (see `DynTool::deserialize_output`).
fn cap_session_image_dimensions(session: &mut ChatSession, max_edge: u32) {
    for node in session.message_nodes.values_mut() {
        cap_message_images(&mut node.message, max_edge);
    }
    for message in &mut session.messages {
        cap_message_images(message, max_edge);
    }
}

/// Cap oversized `ContentBlock::Image` blocks in a single message in place.
fn cap_message_images(message: &mut Message, max_edge: u32) {
    let llm::MessageContent::Structured(blocks) = &mut message.content else {
        return;
    };
    for block in blocks {
        if let llm::ContentBlock::Image {
            media_type, data, ..
        } = block
            && let Some((new_media_type, new_data)) =
                tools_core::cap_base64_image(media_type, data, max_edge)
        {
            *media_type = new_media_type;
            *data = new_data;
        }
    }
}

/// The structured application snapshot riding on a message node's
/// `extension` slot. Holds the plan and the set of active skills as they
/// stood after this message's response — used to reconstruct that state when
/// switching branches.
///
/// Legacy session files stored a bare [`PlanState`] in `extension`; the
/// accessors below transparently read that older shape.
#[derive(Debug, Default, Serialize, Deserialize)]
struct NodeExtension {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan: Option<PlanState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_skills: Option<Vec<String>>,
}

/// Typed access to the per-node application snapshots (plan + active skills)
/// riding on a message node's `extension` slot. A field is only set when it
/// changed in this message's response; used for reconstruction when switching
/// branches.
pub trait MessageNodeExt {
    fn plan_snapshot(&self) -> Option<PlanState>;
    fn set_plan_snapshot(&mut self, plan: PlanState);
    fn active_skills_snapshot(&self) -> Option<Vec<String>>;
    fn set_active_skills_snapshot(&mut self, active_skills: Vec<String>);
}

impl MessageNodeExt for MessageNode {
    fn plan_snapshot(&self) -> Option<PlanState> {
        let value = self.extension.as_ref()?;
        // New combined shape: { "plan": {...}, "active_skills": [...] }.
        if let Ok(ext) = serde_json::from_value::<NodeExtension>(value.clone())
            && (ext.plan.is_some() || ext.active_skills.is_some())
        {
            return ext.plan;
        }
        // Legacy shape: a bare PlanState.
        serde_json::from_value::<PlanState>(value.clone()).ok()
    }

    fn set_plan_snapshot(&mut self, plan: PlanState) {
        let mut ext = self.node_extension();
        ext.plan = Some(plan);
        self.extension = serde_json::to_value(ext).ok();
    }

    fn active_skills_snapshot(&self) -> Option<Vec<String>> {
        let value = self.extension.as_ref()?;
        serde_json::from_value::<NodeExtension>(value.clone())
            .ok()
            .and_then(|ext| ext.active_skills)
    }

    fn set_active_skills_snapshot(&mut self, active_skills: Vec<String>) {
        let mut ext = self.node_extension();
        ext.active_skills = Some(active_skills);
        self.extension = serde_json::to_value(ext).ok();
    }
}

/// Private helper to read the current node extension, normalizing the legacy
/// bare-`PlanState` shape into the combined [`NodeExtension`] so updates to
/// one field preserve the other.
trait NodeExtensionAccess {
    fn node_extension(&self) -> NodeExtension;
}

impl NodeExtensionAccess for MessageNode {
    fn node_extension(&self) -> NodeExtension {
        NodeExtension {
            plan: self.plan_snapshot(),
            active_skills: self.active_skills_snapshot(),
        }
    }
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_executions: Vec<SerializedToolExecution>,
    /// Current session plan (for the active path)
    #[serde(default)]
    pub plan: PlanState,
    /// Names of skills activated on the active path (for progressive
    /// disclosure). Reconstructed from per-node snapshots on branch switch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_skills: Vec<String>,
    /// Whether the plan UI is collapsed for this session
    #[serde(default)]
    pub plan_collapsed: bool,
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

/// Determine whether a linear list of messages represents a session that
/// should be marked as "resumable" — i.e. ends in a user message or in an
/// assistant message with un-answered tool calls.
///
/// Used by both [`ChatSession::is_resumable`] and the agent runner (which
/// only has the linear `message_history` available while running).
pub fn is_resumable_from_messages(messages: &[&Message]) -> bool {
    let Some(last) = messages.last() else {
        return false;
    };

    match last.role {
        llm::MessageRole::User => match &last.content {
            llm::MessageContent::Text(text) => !text.trim().is_empty(),
            llm::MessageContent::Structured(blocks) => !blocks.is_empty(),
        },
        llm::MessageRole::Assistant => match &last.content {
            llm::MessageContent::Structured(blocks) => blocks
                .iter()
                .any(|b| matches!(b, llm::ContentBlock::ToolUse { .. })),
            llm::MessageContent::Text(_) => false,
        },
    }
}

impl ChatSession {
    /// Merge any legacy top-level fields into the nested SessionConfig.
    pub fn ensure_config(&mut self) -> Result<()> {
        if let Some(init_path) = self.legacy_init_path.take() {
            self.config.init_path = Some(init_path);
        }
        if let Some(initial_project) = self.legacy_initial_project.take()
            && !initial_project.is_empty()
        {
            self.config.initial_project = initial_project;
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
            active_skills: Vec::new(),
            plan_collapsed: false,
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
                    extension: None,
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
            if let Some(node) = self.message_nodes.get(&node_id)
                && let Some(plan) = node.plan_snapshot()
            {
                return plan;
            }
        }
        PlanState::default()
    }

    /// Find the active skills for the active path by walking backwards to the
    /// most recent active-skills snapshot.
    pub fn get_active_skills_for_active_path(&self) -> Vec<String> {
        for &node_id in self.active_path.iter().rev() {
            if let Some(node) = self.message_nodes.get(&node_id)
                && let Some(active_skills) = node.active_skills_snapshot()
            {
                return active_skills;
            }
        }
        Vec::new()
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
            extension: None,
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

        // Update the plan and active skills to match the new active path
        self.plan = self.get_plan_for_active_path();
        self.active_skills = self.get_active_skills_for_active_path();

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

    /// The session-list entry describing the current state of this session.
    pub fn metadata(&self) -> ChatMetadata {
        let (total_usage, last_usage, tokens_limit) = calculate_session_usage(self);
        ChatMetadata {
            id: self.id.clone(),
            name: self.name.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            message_count: self.message_count(),
            total_usage,
            last_usage,
            tokens_limit,
            tool_syntax: self.tool_syntax(),
            initial_project: self.initial_project().to_string(),
            branch: self.config.branch.clone(),
            plan_collapsed: self.plan_collapsed,
            is_resumable: self.is_resumable(),
        }
    }

    /// A copy of the session without its conversation: no nodes, no tool
    /// executions, no legacy linear messages.
    fn without_conversation(&self) -> ChatSession {
        ChatSession {
            id: self.id.clone(),
            name: self.name.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            message_nodes: BTreeMap::new(),
            active_path: self.active_path.clone(),
            next_node_id: self.next_node_id,
            messages: Vec::new(),
            tool_executions: Vec::new(),
            plan: self.plan.clone(),
            active_skills: self.active_skills.clone(),
            plan_collapsed: self.plan_collapsed,
            config: self.config.clone(),
            next_request_id: self.next_request_id,
            model_config: self.model_config.clone(),
            legacy_init_path: self.legacy_init_path.clone(),
            legacy_initial_project: self.legacy_initial_project.clone(),
            legacy_tool_syntax: self.legacy_tool_syntax,
            legacy_use_diff_blocks: self.legacy_use_diff_blocks,
            _legacy_working_memory: serde_json::Value::Null,
        }
    }

    /// Merge a running agent's checkpoint. Nodes and journal entries are
    /// replaced by id, so branches and records the run never touched
    /// survive; counters only ever grow.
    pub fn apply_checkpoint(&mut self, checkpoint: &crate::session::SessionCheckpoint<'_>) {
        self.name = checkpoint.name.to_string();
        for node in checkpoint.changed_nodes {
            self.message_nodes.insert(node.id, (*node).clone());
        }
        self.active_path = checkpoint.active_path.to_vec();
        self.next_node_id = self.next_node_id.max(checkpoint.next_node_id);
        // The tree is authoritative once a checkpoint has been applied.
        self.messages.clear();
        for execution in &checkpoint.changed_executions {
            let id = &execution.tool_request.id;
            match self
                .tool_executions
                .iter()
                .position(|entry| &entry.tool_request.id == id)
            {
                Some(index) => self.tool_executions[index] = execution.clone(),
                None => self.tool_executions.push(execution.clone()),
            }
        }
        self.plan = checkpoint.plan.clone();
        self.active_skills = checkpoint.active_skills.to_vec();
        self.next_request_id = self.next_request_id.max(checkpoint.next_request_id);
        self.updated_at = SystemTime::now();
    }

    /// Returns true if the session looks like it failed mid-flight and could
    /// usefully be "resumed" by re-running the agent against the existing
    /// message history.
    ///
    /// This is the case when the active conversation ends in either:
    ///   - a user message that the agent never responded to, or
    ///   - an assistant message that contains tool-use blocks but has no
    ///     matching tool-result follow-up message (i.e. the agent crashed or
    ///     was killed before executing / returning the tool result).
    ///
    /// A session that ends in a plain assistant text reply is considered
    /// "complete" and is not resumable.
    pub fn is_resumable(&self) -> bool {
        let messages: Vec<&Message> = self.get_active_messages();
        is_resumable_from_messages(messages.as_slice())
    }

    /// Check if the session has any branches.
    #[allow(dead_code)] // Used by tests
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

// The serialized tool execution lives in the agent core.
pub use agent_core::SerializedToolExecution;

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
    /// The git branch the session works on, when it was switched to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Whether the plan UI is collapsed for this session
    #[serde(default)]
    pub plan_collapsed: bool,
    /// Whether the session looks resumable (idle but ended in a user message
    /// or in an assistant message with un-answered tool calls).
    ///
    /// This flag is recomputed every time the session is saved, so it is not
    /// authoritative on disk — the UI should treat it as a hint and the
    /// agent itself never relies on it.
    #[serde(default)]
    pub is_resumable: bool,
}

#[derive(Clone)]
pub struct FileSessionPersistence {
    layout: SessionLayout,
}

impl FileSessionPersistence {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let root_dir = crate::config_dir::data_dir();
        info!("Storing sessions in: {:?}", root_dir.to_path_buf());
        Self::new_with_root_dir(root_dir)
    }

    /// Construct a persistence instance rooted at a custom directory;
    /// sessions are stored in `<root_dir>/sessions`. Used by tests to
    /// isolate state and by embedders (e.g. pal) that keep their session
    /// store outside the code-assistant data directory.
    pub fn new_with_root_dir(root_dir: PathBuf) -> Self {
        Self {
            layout: SessionLayout::new(root_dir.join("sessions")),
        }
    }

    /// Where the files of each session live.
    pub fn layout(&self) -> &SessionLayout {
        &self.layout
    }

    fn ensure_chats_dir(&self) -> Result<PathBuf> {
        let chats_dir = self.layout.sessions_dir().to_path_buf();
        if !chats_dir.exists() {
            std::fs::create_dir_all(&chats_dir)?;
        }
        Ok(chats_dir)
    }

    fn metadata_file_path(&self) -> Result<PathBuf> {
        let chats_dir = self.ensure_chats_dir()?;
        Ok(chats_dir.join("metadata.json"))
    }

    fn metadata_lock_path(&self) -> Result<PathBuf> {
        let chats_dir = self.ensure_chats_dir()?;
        Ok(chats_dir.join("metadata.lock"))
    }

    fn lifecycle_file_path(&self) -> Result<PathBuf> {
        Ok(self.ensure_chats_dir()?.join("lifecycle.json"))
    }

    fn lifecycle_lock_path(&self) -> Result<PathBuf> {
        Ok(self.ensure_chats_dir()?.join("lifecycle.lock"))
    }

    /// Reserve the ID of a new session of a project (see
    /// [`SessionLayout::allocate_session_id`]).
    pub fn allocate_session_id(&self, project_root: Option<&std::path::Path>) -> Result<String> {
        self.layout
            .allocate_session_id(project_root, chrono::Local::now().date_naive())
    }

    /// Update an existing session under a cross-process, per-entry lock.
    /// The closure sees the latest on-disk entry; an error leaves it unchanged.
    /// Only what the closure changed is appended to the journal. Lock order
    /// is entry -> metadata. This is separate from the long-lived agent lock
    /// so settings can still change during a run. Do not re-enter
    /// persistence from the closure. Lock files must never be unlinked.
    pub fn update_entry(
        &mut self,
        session_id: &str,
        update: impl FnOnce(&mut ChatSession) -> Result<()>,
    ) -> Result<ChatSession> {
        let _lock = self.lock_existing_entry(session_id)?;
        let folded = self.read_journal(session_id)?;
        let mut before = folded.session;
        self.resolve_blobs(&mut before)?;
        before.ensure_config()?;
        let mut after = before.clone();
        update(&mut after)?;
        anyhow::ensure!(after.id == session_id, "Cannot change session identity");
        after.ensure_config()?;
        let records = journal::diff(&before, &after)?;
        self.append(session_id, records, folded.records, &after)?;
        self.store_metadata(after.metadata())?;
        Ok(after)
    }

    /// Merge a running agent's checkpoint (see
    /// [`ChatSession::apply_checkpoint`]) and return the session's new
    /// metadata. Appends the changed nodes and executions and, when needed, a
    /// new header; tool results already stored are neither read nor written.
    pub fn commit_checkpoint(
        &mut self,
        checkpoint: &crate::session::SessionCheckpoint<'_>,
    ) -> Result<ChatMetadata> {
        let session_id = checkpoint.session_id;
        let _lock = self.lock_existing_entry(session_id)?;
        let folded = self.read_journal(session_id)?;
        let mut session = folded.session;
        let header_before = serde_json::to_vec(&journal::header(&session))?;
        session.apply_checkpoint(checkpoint);

        let mut records = Vec::new();
        let header = journal::header(&session);
        if serde_json::to_vec(&header)? != header_before {
            records.push(header);
        }
        records.extend(
            checkpoint
                .changed_nodes
                .iter()
                .map(|node| journal::Record::Node {
                    node: (*node).clone(),
                }),
        );
        records.extend(
            checkpoint
                .changed_executions
                .iter()
                .map(|exec| journal::Record::Exec { exec: exec.clone() }),
        );
        self.append(session_id, records, folded.records, &session)?;

        let metadata = session.metadata();
        self.store_metadata(metadata.clone())?;
        Ok(metadata)
    }

    /// Store a new session, in the folder reserved by
    /// [`Self::allocate_session_id`] or a new one. An existing entry is never
    /// replaced: a supplied snapshot may be stale, so changes go through
    /// `update_entry`.
    pub fn create_chat_session(&mut self, session: &ChatSession) -> Result<()> {
        let _lock = lock_exclusive(&self.layout.entry_lock(&session.id)?)?;
        let path = self.layout.journal(&session.id)?;
        anyhow::ensure!(!path.exists(), "Session already exists: {}", session.id);

        let mut session = session.clone();
        session.ensure_config()?;
        self.externalize_blobs(&session.id, &mut session.tool_executions)?;
        journal::write(&path, &journal::snapshot(&session))?;
        self.store_metadata(session.metadata())
    }

    pub fn load_chat_session(&self, session_id: &str) -> Result<Option<ChatSession>> {
        let path = self.layout.journal(session_id)?;
        debug!("Loading chat session from {}", path.display());
        let Some(folded) = journal::read(&path)? else {
            return Ok(None);
        };
        let mut session = folded.session;
        self.resolve_blobs(&mut session)?;
        session.ensure_config()?;
        // Re-check image dimensions on load: sessions persisted before image
        // capping (or by an older version) may carry oversized images that a
        // provider would reject, making the session unresumable. This corrects
        // user-attached images in the conversation tree; oversized tool-result
        // images are corrected when their execution records are deserialized
        // (see `DynTool::deserialize_output`).
        cap_session_image_dimensions(&mut session, tools_core::MAX_IMAGE_EDGE);
        Ok(Some(session))
    }

    /// Replace a session's journal with a snapshot, ignoring the entry lock:
    /// lets a test play the process that holds it.
    #[cfg(test)]
    pub(crate) fn overwrite_journal_unlocked(&self, session: &ChatSession) -> Result<()> {
        journal::write(
            &self.layout.journal(&session.id)?,
            &journal::snapshot(session),
        )
    }

    /// Lock the entry of a session that must exist. Locking alone would
    /// create the folder of a missing session.
    fn lock_existing_entry(&self, session_id: &str) -> Result<FileLockGuard> {
        anyhow::ensure!(
            self.layout.journal(session_id)?.exists(),
            "Session not found: {session_id}"
        );
        lock_exclusive(&self.layout.entry_lock(session_id)?)
    }

    /// The folded journal of an existing session, tool results unresolved.
    fn read_journal(&self, session_id: &str) -> Result<journal::Folded> {
        journal::read(&self.layout.journal(session_id)?)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))
    }

    fn resolve_blobs(&self, session: &mut ChatSession) -> Result<()> {
        let blobs = blobs::BlobStore::new(self.layout.blobs_dir(&session.id)?);
        for execution in &mut session.tool_executions {
            execution.result_json = blobs.resolve(std::mem::take(&mut execution.result_json))?;
        }
        Ok(())
    }

    fn externalize_blobs(
        &self,
        session_id: &str,
        executions: &mut [SerializedToolExecution],
    ) -> Result<()> {
        let blobs = blobs::BlobStore::new(self.layout.blobs_dir(session_id)?);
        for execution in executions {
            execution.result_json =
                blobs.externalize(std::mem::take(&mut execution.result_json))?;
        }
        Ok(())
    }

    /// Append records to a session's journal of `records_before` lines,
    /// moving large tool results to blobs first, and compact the journal once
    /// it has grown too long for the resulting session. The caller holds the
    /// entry lock.
    fn append(
        &self,
        session_id: &str,
        mut records: Vec<journal::Record>,
        records_before: usize,
        session: &ChatSession,
    ) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let blobs = blobs::BlobStore::new(self.layout.blobs_dir(session_id)?);
        for record in &mut records {
            if let journal::Record::Exec { exec } = record {
                exec.result_json = blobs.externalize(std::mem::take(&mut exec.result_json))?;
            }
        }
        let path = self.layout.journal(session_id)?;
        journal::append(&path, &records)?;

        if journal::needs_compaction(records_before + records.len(), session) {
            debug!("Compacting {}", path.display());
            // Read back with tool results unresolved: the snapshot keeps
            // their blob references.
            let folded = self.read_journal(session_id)?;
            journal::write(&path, &journal::snapshot(&folded.session))?;
            blobs.retain_referenced(&folded.session.tool_executions)?;
        }
        Ok(())
    }

    /// Put a session's metadata into the index, under the metadata lock so
    /// concurrent processes don't lose each other's changes. Nothing is
    /// written when the entry is unchanged.
    fn store_metadata(&self, metadata: ChatMetadata) -> Result<()> {
        let _lock = lock_exclusive(&self.metadata_lock_path()?)?;
        let metadata_path = self.metadata_file_path()?;
        let mut metadata_list: Vec<ChatMetadata> = if metadata_path.exists() {
            let content = std::fs::read_to_string(&metadata_path)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Vec::new()
        };
        match metadata_list.iter_mut().find(|m| m.id == metadata.id) {
            Some(existing) if *existing == metadata => return Ok(()),
            Some(existing) => *existing = metadata,
            None => metadata_list.push(metadata),
        }
        atomic_write_json(&metadata_path, &metadata_list)
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
        metadata_list.sort_by_key(|item| std::cmp::Reverse(item.updated_at));

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
        let session_dir = self.layout.session_dir(session_id)?;
        if session_dir.is_dir() {
            // The folder goes with everything in it, the held entry lock
            // included: a process still waiting on that lock finds no
            // session afterwards, and `update_entry` refuses to recreate it.
            let _entry_lock = lock_exclusive(&self.layout.entry_lock(session_id)?)?;
            debug!("Deleting session folder {}", session_dir.display());
            std::fs::remove_dir_all(&session_dir)?;
        }

        // Update metadata under lock
        let metadata_lock_path = self.metadata_lock_path()?;
        let _lock = lock_exclusive(&metadata_lock_path)?;

        let metadata_path = self.metadata_file_path()?;
        if metadata_path.exists() {
            let content = std::fs::read_to_string(&metadata_path)?;
            let mut metadata_list: Vec<ChatMetadata> =
                serde_json::from_str(&content).unwrap_or_default();

            metadata_list.retain(|m| m.id != session_id);

            atomic_write_json(&metadata_path, &metadata_list)?;
        }
        drop(_lock);

        self.remove_lifecycle(session_id)?;

        Ok(())
    }

    // ── Session lifecycle (`lifecycle.json`) ────────────────────────────
    //
    // Visits and settlement live next to the index, not in the session
    // file: a visit must not rewrite a multi-megabyte conversation. The file
    // is a map from session id to [`SessionLifecycle`], guarded by its own
    // cross-process lock.

    fn read_lifecycles_unlocked(&self) -> Result<HashMap<String, SessionLifecycle>> {
        let path = self.lifecycle_file_path()?;
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let content = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&content).unwrap_or_else(|e| {
            warn!("Failed to parse {}: {e}; starting empty", path.display());
            HashMap::new()
        }))
    }

    /// The lifecycle of every session that has one. Sessions absent from the
    /// map have the default lifecycle.
    pub fn load_lifecycles(&self) -> Result<HashMap<String, SessionLifecycle>> {
        let _lock = lock_exclusive(&self.lifecycle_lock_path()?)?;
        self.read_lifecycles_unlocked()
    }

    /// Change one session's lifecycle under the cross-process lock. The
    /// closure sees the latest on-disk record (or the default) and the
    /// updated record is returned. Nothing is written when the closure
    /// leaves the record unchanged.
    pub fn update_lifecycle(
        &self,
        session_id: &str,
        update: impl FnOnce(&mut SessionLifecycle),
    ) -> Result<SessionLifecycle> {
        let _lock = lock_exclusive(&self.lifecycle_lock_path()?)?;
        let mut lifecycles = self.read_lifecycles_unlocked()?;
        let before = lifecycles.get(session_id).cloned().unwrap_or_default();
        let mut lifecycle = before.clone();
        update(&mut lifecycle);
        if lifecycle != before {
            lifecycles.insert(session_id.to_string(), lifecycle.clone());
            atomic_write_json(&self.lifecycle_file_path()?, &lifecycles)?;
        }
        Ok(lifecycle)
    }

    /// Change several sessions' lifecycles under one lock and one write.
    /// The closure runs per session with the latest record (or the
    /// default); unchanged records are not stored. Returns the records
    /// that changed.
    pub fn update_lifecycles(
        &self,
        session_ids: &[String],
        mut update: impl FnMut(&str, &mut SessionLifecycle),
    ) -> Result<Vec<(String, SessionLifecycle)>> {
        let _lock = lock_exclusive(&self.lifecycle_lock_path()?)?;
        let mut lifecycles = self.read_lifecycles_unlocked()?;
        let mut changed = Vec::new();
        for session_id in session_ids {
            let before = lifecycles.get(session_id).cloned().unwrap_or_default();
            let mut lifecycle = before.clone();
            update(session_id, &mut lifecycle);
            if lifecycle != before {
                lifecycles.insert(session_id.clone(), lifecycle.clone());
                changed.push((session_id.clone(), lifecycle));
            }
        }
        if !changed.is_empty() {
            atomic_write_json(&self.lifecycle_file_path()?, &lifecycles)?;
        }
        Ok(changed)
    }

    fn remove_lifecycle(&self, session_id: &str) -> Result<()> {
        let _lock = lock_exclusive(&self.lifecycle_lock_path()?)?;
        let mut lifecycles = self.read_lifecycles_unlocked()?;
        if lifecycles.remove(session_id).is_some() {
            atomic_write_json(&self.lifecycle_file_path()?, &lifecycles)?;
        }
        Ok(())
    }

    /// Rebuild metadata from existing session files (used when metadata file is corrupted)
    fn rebuild_metadata_from_sessions(&self) -> Result<Vec<ChatMetadata>> {
        let mut metadata_list = Vec::new();

        for session_id in self.layout.session_ids()? {
            if let Ok(Some(session)) = self.load_chat_session(&session_id) {
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
                    branch: session.config.branch.clone(),
                    plan_collapsed: session.plan_collapsed,
                    is_resumable: session.is_resumable(),
                };

                metadata_list.push(metadata);
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

    /// Helper method to save metadata list to file.
    ///
    /// Acquires the metadata lock and writes atomically.
    fn save_metadata_list(&self, metadata_list: &[ChatMetadata]) -> Result<()> {
        let metadata_lock_path = self.metadata_lock_path()?;
        let _lock = lock_exclusive(&metadata_lock_path)?;

        let metadata_path = self.metadata_file_path()?;
        atomic_write_json(&metadata_path, metadata_list)?;
        Ok(())
    }

    pub fn get_latest_session_id(&self) -> Result<Option<String>> {
        let sessions = self.list_chat_sessions()?;
        Ok(sessions.first().map(|s| s.id.clone()))
    }
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DraftAttachment {
    #[serde(rename = "text")]
    Text { content: String },
    #[serde(rename = "image")]
    Image {
        content: String,
        mime_type: String,
        /// Image dimensions (optional, for display purposes)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
    #[serde(rename = "file")]
    File {
        content: String,
        filename: String,
        mime_type: String,
    },
}

/// What the composer of a session holds while the user has not sent it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionDraft {
    pub session_id: String,
    /// The main message text that the user types
    pub message: String,
    /// Additional attachments (images, files, etc.)
    pub attachments: Vec<DraftAttachment>,
    /// When editing an existing message, this is the parent node where the new
    /// branch will be created. `Some(_)` means the draft is in "edit mode" and
    /// the UI should restore the editing banner and truncated transcript when
    /// the session is connected. Defaults to `None` for backward compatibility
    /// with drafts written before this field existed.
    #[serde(default)]
    pub editing_branch_parent_id: Option<NodeId>,
}

impl SessionDraft {
    /// A draft is empty only when it carries no text, no attachments AND no
    /// edit state: an in-progress edit (even with empty text) must persist so
    /// the editing banner can be restored when reconnecting to the session.
    pub fn is_empty(&self) -> bool {
        self.message.is_empty()
            && self.attachments.is_empty()
            && self.editing_branch_parent_id.is_none()
    }
}

/// Where the drafts of all sessions are kept.
pub trait DraftStore: Send + Sync {
    /// The stored draft of a session, if there is one.
    fn load(&self, session_id: &str) -> Result<Option<SessionDraft>>;

    /// Store a draft, replacing the session's previous one.
    fn save(&self, draft: &SessionDraft) -> Result<()>;

    /// Remove the stored draft of a session. Removing a missing draft is not
    /// an error.
    fn delete(&self, session_id: &str) -> Result<()>;
}

/// Drafts as `draft.json` files in the session folders.
#[derive(Debug, Clone)]
pub struct FileDraftStore {
    layout: SessionLayout,
}

impl FileDraftStore {
    pub fn new(layout: SessionLayout) -> Self {
        Self { layout }
    }
}

impl DraftStore for FileDraftStore {
    fn load(&self, session_id: &str) -> Result<Option<SessionDraft>> {
        let file_path = self.layout.draft(session_id)?;
        let json_content = match std::fs::read_to_string(&file_path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(serde_json::from_str(&json_content)?))
    }

    fn save(&self, draft: &SessionDraft) -> Result<()> {
        // A deleted session keeps no draft; writing would recreate its folder.
        if !self.layout.session_dir(&draft.session_id)?.is_dir() {
            return Ok(());
        }
        // Written atomically, so concurrent saves of one session never leave
        // a torn file behind.
        atomic_write_json(&self.layout.draft(&draft.session_id)?, draft)
    }

    fn delete(&self, session_id: &str) -> Result<()> {
        match std::fs::remove_file(self.layout.draft(session_id)?) {
            Ok(()) => {
                debug!("Cleared draft for session: {}", session_id);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionConfig;
    use crate::types::{PlanItem, PlanItemPriority, PlanItemStatus};
    use base64::Engine as _;
    use tempfile::tempdir;

    fn execution(id: &str, result_json: serde_json::Value) -> SerializedToolExecution {
        SerializedToolExecution {
            tool_request: agent_core::ToolRequest {
                id: id.into(),
                name: "read_files".into(),
                input: serde_json::json!({}),
                start_offset: None,
                end_offset: None,
            },
            result_json,
            tool_name: "read_files".into(),
        }
    }

    #[test]
    fn large_tool_results_live_in_blobs_outside_the_record() {
        let dir = tempdir().unwrap();
        let mut persistence = FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf());
        let mut session =
            ChatSession::new_empty("p/s".into(), "s".into(), SessionConfig::default(), None);
        let large = serde_json::json!({ "content": "x".repeat(100_000) });
        let small = serde_json::json!({ "content": "short" });
        session.tool_executions =
            vec![execution("a", large.clone()), execution("b", small.clone())];
        persistence.create_chat_session(&session).unwrap();

        let record = std::fs::read_to_string(persistence.layout().journal("p/s").unwrap()).unwrap();
        assert!(record.len() < 10_000, "record is {} bytes", record.len());
        assert!(record.contains("short"));
        let blobs = std::fs::read_dir(persistence.layout().blobs_dir("p/s").unwrap()).unwrap();
        assert_eq!(blobs.count(), 1);

        let loaded = persistence.load_chat_session("p/s").unwrap().unwrap();
        assert_eq!(loaded.tool_executions[0].result_json, large);
        assert_eq!(loaded.tool_executions[1].result_json, small);

        // Updating resolves and stores again without losing the result.
        persistence
            .update_entry("p/s", |session| {
                session.name = "renamed".into();
                Ok(())
            })
            .unwrap();
        let loaded = persistence.load_chat_session("p/s").unwrap().unwrap();
        assert_eq!(loaded.tool_executions[0].result_json, large);
    }

    fn journal_lines(persistence: &FileSessionPersistence, id: &str) -> usize {
        std::fs::read_to_string(persistence.layout().journal(id).unwrap())
            .unwrap()
            .lines()
            .count()
    }

    fn blob_count(persistence: &FileSessionPersistence, id: &str) -> usize {
        std::fs::read_dir(persistence.layout().blobs_dir(id).unwrap())
            .map(|entries| entries.count())
            .unwrap_or(0)
    }

    #[test]
    fn updates_append_only_what_changed() {
        let dir = tempdir().unwrap();
        let mut persistence = FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf());
        let mut session =
            ChatSession::new_empty("p/s".into(), "s".into(), SessionConfig::default(), None);
        session.add_message(Message::new_user("hello"));
        session.tool_executions = vec![execution(
            "a",
            serde_json::json!({ "c": "x".repeat(10_000) }),
        )];
        persistence.create_chat_session(&session).unwrap();
        assert_eq!(journal_lines(&persistence, "p/s"), 3);

        persistence
            .update_entry("p/s", |session| {
                session.plan_collapsed = true;
                Ok(())
            })
            .unwrap();
        assert_eq!(journal_lines(&persistence, "p/s"), 4);

        // Nothing changed, nothing written.
        persistence.update_entry("p/s", |_| Ok(())).unwrap();
        assert_eq!(journal_lines(&persistence, "p/s"), 4);

        let loaded = persistence.load_chat_session("p/s").unwrap().unwrap();
        assert!(loaded.plan_collapsed);
        assert_eq!(loaded.message_count(), 1);
    }

    #[test]
    fn checkpoints_neither_read_nor_write_stored_tool_results() {
        let dir = tempdir().unwrap();
        let mut persistence = FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf());
        let mut session =
            ChatSession::new_empty("p/s".into(), "s".into(), SessionConfig::default(), None);
        session.tool_executions = vec![execution(
            "a",
            serde_json::json!({ "c": "x".repeat(10_000) }),
        )];
        persistence.create_chat_session(&session).unwrap();
        // Without the blob, any attempt to read the stored result would fail.
        std::fs::remove_dir_all(persistence.layout().blobs_dir("p/s").unwrap()).unwrap();

        let node = MessageNode {
            id: 1,
            message: Message::new_user("next"),
            parent_id: None,
            created_at: SystemTime::now(),
            extension: None,
        };
        let large = serde_json::json!({ "c": "y".repeat(10_000) });
        let metadata = persistence
            .commit_checkpoint(&crate::session::SessionCheckpoint {
                session_id: "p/s",
                name: "named by the run",
                changed_nodes: &[&node],
                active_path: &[1],
                next_node_id: 2,
                changed_executions: vec![execution("b", large)],
                plan: &PlanState::default(),
                active_skills: &[],
                next_request_id: 1,
            })
            .unwrap();

        assert_eq!(metadata.name, "named by the run");
        assert_eq!(metadata.message_count, 1);
        assert_eq!(blob_count(&persistence, "p/s"), 1);
        // header + node + exec appended to header + exec
        assert_eq!(journal_lines(&persistence, "p/s"), 5);
    }

    #[test]
    fn a_long_journal_is_compacted_and_drops_unreferenced_blobs() {
        let dir = tempdir().unwrap();
        let mut persistence = FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf());
        let mut session =
            ChatSession::new_empty("p/s".into(), "s".into(), SessionConfig::default(), None);
        session.tool_executions = vec![execution(
            "a",
            serde_json::json!({ "c": "0".repeat(10_000) }),
        )];
        persistence.create_chat_session(&session).unwrap();

        // Re-recording the execution replaces its result; old blobs pile up
        // until compaction removes them.
        for round in 1..40 {
            persistence
                .update_entry("p/s", |session| {
                    session.tool_executions[0].result_json =
                        serde_json::json!({ "c": round.to_string().repeat(10_000) });
                    Ok(())
                })
                .unwrap();
        }

        assert!(journal_lines(&persistence, "p/s") <= 2 * 2 + 16 + 1);
        assert!(blob_count(&persistence, "p/s") < 20);
        let loaded = persistence.load_chat_session("p/s").unwrap().unwrap();
        assert_eq!(
            loaded.tool_executions[0].result_json,
            serde_json::json!({ "c": "39".repeat(10_000) })
        );
    }

    #[test]
    fn checkpoint_update_entry_serializes_independent_persistence_instances() {
        let dir = tempdir().unwrap();
        let mut persistence = FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf());
        persistence
            .create_chat_session(&ChatSession::new_empty(
                "shared".into(),
                "shared".into(),
                SessionConfig::default(),
                None,
            ))
            .unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(4));
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let root = dir.path().to_path_buf();
                let start = start.clone();
                scope.spawn(move || {
                    let mut persistence = FileSessionPersistence::new_with_root_dir(root);
                    start.wait();
                    for _ in 0..10 {
                        persistence
                            .update_entry("shared", |session| {
                                let previous = session.next_request_id;
                                std::thread::sleep(std::time::Duration::from_millis(1));
                                session.next_request_id = previous + 1;
                                session.add_message(Message::new_user("concurrent append"));
                                Ok(())
                            })
                            .unwrap();
                    }
                });
            }
        });
        let saved = persistence.load_chat_session("shared").unwrap().unwrap();
        assert_eq!(saved.next_request_id, 41);
        assert_eq!(saved.message_count(), 40);
        assert_eq!(saved.get_active_messages().len(), 40);
        assert_eq!(
            persistence
                .get_chat_session_metadata("shared")
                .unwrap()
                .unwrap()
                .message_count,
            40
        );
    }

    #[test]
    fn checkpoint_update_entry_error_does_not_write_or_create() {
        let dir = tempdir().unwrap();
        let mut persistence = FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf());
        assert!(persistence.update_entry("missing", |_| Ok(())).is_err());
        assert!(persistence.load_chat_session("missing").unwrap().is_none());
        persistence
            .create_chat_session(&ChatSession::new_empty(
                "existing".into(),
                "original".into(),
                SessionConfig::default(),
                None,
            ))
            .unwrap();
        let before = std::fs::read(persistence.layout().journal("existing").unwrap()).unwrap();
        assert!(
            persistence
                .update_entry("existing", |session| {
                    session.name = "not committed".into();
                    anyhow::bail!("abort update")
                })
                .is_err()
        );
        assert_eq!(
            before,
            std::fs::read(persistence.layout().journal("existing").unwrap()).unwrap()
        );
        // The failed transaction also released its lock.
        persistence
            .update_entry("existing", |session| {
                session.name = "committed".into();
                Ok(())
            })
            .unwrap();
    }

    fn oversized_png_base64(width: u32, height: u32) -> String {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([1, 2, 3, 255]),
        ));
        let mut bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn png_dimensions(base64_data: &str) -> (u32, u32) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(base64_data)
            .unwrap();
        image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap()
    }

    #[test]
    fn cap_message_images_shrinks_oversized_user_image() {
        let mut message = Message::new_user_content(vec![
            llm::ContentBlock::new_text("look at this"),
            llm::ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: oversized_png_base64(9000, 3000),
                start_time: None,
                end_time: None,
            },
        ]);

        cap_message_images(&mut message, 1568);

        match &message.content {
            llm::MessageContent::Structured(blocks) => match &blocks[1] {
                llm::ContentBlock::Image { data, .. } => {
                    let (w, h) = png_dimensions(data);
                    assert_eq!(w, 1568);
                    assert!(h <= 1568);
                }
                other => panic!("expected image block, got {other:?}"),
            },
            other => panic!("expected structured content, got {other:?}"),
        }
    }

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
    fn test_node_extension_backward_compat_and_combined() {
        // Legacy session files stored a bare PlanState in the extension slot.
        let legacy_plan = PlanState {
            entries: Vec::new(),
            meta: None,
        };
        let mut node = MessageNode {
            id: 1,
            message: Message::new_assistant("hi"),
            parent_id: None,
            created_at: SystemTime::now(),
            extension: Some(serde_json::to_value(&legacy_plan).unwrap()),
        };
        assert!(node.plan_snapshot().is_some());
        assert!(node.active_skills_snapshot().is_none());

        // Setting active skills must preserve the existing plan.
        node.set_active_skills_snapshot(vec!["pdf".to_string()]);
        assert_eq!(node.active_skills_snapshot(), Some(vec!["pdf".to_string()]));
        assert!(node.plan_snapshot().is_some());

        // Setting the plan must preserve the active skills.
        node.set_plan_snapshot(PlanState::default());
        assert_eq!(node.active_skills_snapshot(), Some(vec!["pdf".to_string()]));
    }

    #[test]
    fn test_switch_branch_reconstructs_active_skills() {
        let mut session = ChatSession::new_empty(
            "test".to_string(),
            "Test".to_string(),
            SessionConfig::default(),
            None,
        );

        session.add_message(Message::new_user("Hello")); // node 1
        session.add_message(Message::new_assistant("Hi!")); // node 2
        session
            .message_nodes
            .get_mut(&2)
            .unwrap()
            .set_active_skills_snapshot(vec!["alpha".to_string()]);
        session.add_message(Message::new_user("Original followup")); // node 3

        // Branch from node 1 with a different active skill.
        session.add_message_with_parent(Message::new_user("Alternative"), Some(1)); // node 4
        session.add_message(Message::new_assistant("Alt response")); // node 5
        session
            .message_nodes
            .get_mut(&5)
            .unwrap()
            .set_active_skills_snapshot(vec!["beta".to_string()]);
        session.active_skills = session.get_active_skills_for_active_path();
        assert_eq!(session.active_skills, vec!["beta".to_string()]);

        // Switching to the original branch restores its active skills.
        session.switch_branch(3).expect("switch branch");
        assert_eq!(session.active_path, vec![1, 2, 3]);
        assert_eq!(session.active_skills, vec!["alpha".to_string()]);

        // Switching back to the alternative branch restores its skills.
        session.switch_branch(5).expect("switch branch");
        assert_eq!(session.active_path, vec![1, 4, 5]);
        assert_eq!(session.active_skills, vec!["beta".to_string()]);
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
            active_skills: Vec::new(),
            plan_collapsed: false,
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

    #[test]
    fn lifecycle_round_trips_and_leaves_with_its_session() {
        let dir = tempdir().unwrap();
        let mut persistence = FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf());
        persistence
            .create_chat_session(&ChatSession::new_empty(
                "s1".into(),
                "s1".into(),
                SessionConfig::default(),
                None,
            ))
            .unwrap();
        assert!(persistence.load_lifecycles().unwrap().is_empty());

        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        let updated = persistence
            .update_lifecycle("s1", |lifecycle| lifecycle.visit(now))
            .unwrap();
        assert_eq!(updated.last_visited_at, Some(now));

        let other = FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf());
        let loaded = other.load_lifecycles().unwrap();
        assert_eq!(loaded.get("s1"), Some(&updated));

        persistence.delete_chat_session("s1").unwrap();
        assert!(persistence.load_lifecycles().unwrap().is_empty());
    }

    #[test]
    fn an_unchanged_lifecycle_update_writes_nothing() {
        let dir = tempdir().unwrap();
        let persistence = FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf());
        persistence.update_lifecycle("ghost", |_| {}).unwrap();
        assert!(!dir.path().join("sessions").join("lifecycle.json").exists());
    }

    #[test]
    fn metadata_carries_the_session_branch() {
        let dir = tempdir().unwrap();
        let mut persistence = FileSessionPersistence::new_with_root_dir(dir.path().to_path_buf());
        let config = SessionConfig {
            branch: Some("feature/x".into()),
            ..SessionConfig::default()
        };
        persistence
            .create_chat_session(&ChatSession::new_empty(
                "s1".into(),
                "s1".into(),
                config,
                None,
            ))
            .unwrap();
        let listed = persistence.list_chat_sessions().unwrap();
        assert_eq!(listed[0].branch.as_deref(), Some("feature/x"));
    }

    fn draft(session_id: &str, message: &str) -> SessionDraft {
        SessionDraft {
            session_id: session_id.into(),
            message: message.into(),
            attachments: Vec::new(),
            editing_branch_parent_id: None,
        }
    }

    #[test]
    fn file_draft_store_round_trips_and_deletes() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("p/s")).unwrap();
        let store = FileDraftStore::new(SessionLayout::new(dir.path().to_path_buf()));
        assert_eq!(store.load("p/s").unwrap(), None);

        store.save(&draft("p/s", "first")).unwrap();
        store.save(&draft("p/s", "second")).unwrap();
        assert_eq!(store.load("p/s").unwrap(), Some(draft("p/s", "second")));
        assert!(dir.path().join("p/s/draft.json").exists());

        store.delete("p/s").unwrap();
        assert_eq!(store.load("p/s").unwrap(), None);
        // Deleting a missing draft is fine.
        store.delete("p/s").unwrap();
    }

    #[test]
    fn file_draft_store_does_not_recreate_a_deleted_session() {
        let dir = tempdir().unwrap();
        let store = FileDraftStore::new(SessionLayout::new(dir.path().to_path_buf()));
        store.save(&draft("gone", "text")).unwrap();
        assert!(!dir.path().join("gone").exists());
    }

    #[test]
    fn file_draft_store_reads_drafts_with_timestamps() {
        // Drafts written before the timestamps were dropped still load.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("s")).unwrap();
        std::fs::write(
            dir.path().join("s/draft.json"),
            r#"{"session_id":"s","created_at":{"secs_since_epoch":1,"nanos_since_epoch":0},
                "updated_at":{"secs_since_epoch":2,"nanos_since_epoch":0},
                "message":"hello","attachments":[]}"#,
        )
        .unwrap();
        let store = FileDraftStore::new(SessionLayout::new(dir.path().to_path_buf()));
        assert_eq!(store.load("s").unwrap(), Some(draft("s", "hello")));
    }
}
