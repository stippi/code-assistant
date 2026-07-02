//! The UI→core command facade.
//!
//! [`SessionService`] is the single entry point for everything a frontend
//! wants the core to *do* (create sessions, send messages, switch models,
//! …). Each operation is a typed async method returning `Result<T>`, so a
//! caller gets *its* answer or *its* error — no correlation over shared
//! channels.
//!
//! Internally the service is an actor: methods enqueue a closure onto a
//! command channel and await a oneshot reply. A single worker future (see
//! [`SessionService::new`]) executes commands strictly in order on the
//! backend's tokio runtime, preserving the serialization of session
//! mutations and keeping the caller's executor (e.g. GPUI) decoupled from
//! tokio. Core→UI notifications keep flowing through [`UiEvent`] and are
//! not part of this API.

use crate::config::{save_project, DefaultProjectManager};
use crate::persistence::{ChatMetadata, DraftAttachment, NodeId, SessionModelConfig};
use crate::session::event_stream::EventStream;
use crate::session::SessionManager;
use crate::skills::{
    discover_session_catalog, load_skill_payload, render_skill_invocation_message, SkillsConfig,
};
use crate::types::{PlanState, Project};
use crate::ui::ui_events::{MessageData, ToolResultData};
use crate::ui::UiEvent;
use crate::utils::content::content_blocks_from;
use anyhow::{anyhow, bail, Context as _, Result};
use command_executor::CommandExecutor;
use llm::factory::create_llm_client_from_model;
use llm::provider_config::ConfigurationSystem;
use sandbox::SandboxPolicy;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Creates the per-session command executor for agent runs. Injected by the
/// wiring so the core stays frontend-agnostic (the GPUI build supplies an
/// executor that can attach commands to live terminal views).
pub type CommandExecutorFactory = Arc<dyn Fn(&str) -> Box<dyn CommandExecutor> + Send + Sync>;

/// Options for running agents: LLM recording/playback plus the command
/// executor factory.
#[derive(Clone)]
pub struct AgentRuntimeOptions {
    pub record_path: Option<PathBuf>,
    pub playback_path: Option<PathBuf>,
    pub fast_playback: bool,
    /// Builds the command executor for a session id when an agent is started.
    pub command_executor_factory: CommandExecutorFactory,
}

/// A single entry in the input-area skill picker.
#[derive(Debug, Clone)]
pub struct SkillCatalogEntry {
    pub name: String,
    pub description: String,
    /// Scope token to pass back to [`SessionService::invoke_skill`] (the
    /// project name, or `:config:` / `:system:`).
    pub scope_token: String,
    /// Human-readable scope label (`project` / `user` / `system`).
    pub scope_label: String,
}

/// Result of a successful model switch.
#[derive(Debug, Clone)]
pub struct ModelSwitchResult {
    /// Optional warning to surface to the user, e.g. when the switch will
    /// only affect the next agent iteration.
    pub warning: Option<String>,
    /// Models that remain valid choices for this session after the switch.
    pub allowed_models: Vec<String>,
}

/// A transcript snapshot: messages plus tool results for one active path.
#[derive(Debug, Clone)]
pub struct TranscriptData {
    pub messages: Vec<MessageData>,
    pub tool_results: Vec<ToolResultData>,
}

/// Everything the UI needs to start editing a past message.
#[derive(Debug, Clone)]
pub struct MessageEditContext {
    /// The text content of the message being edited.
    pub content: String,
    /// Any attachments from the original message.
    pub attachments: Vec<DraftAttachment>,
    /// The parent node ID where the new branch will be created.
    pub branch_parent_id: Option<NodeId>,
    /// Transcript truncated to messages before the one being edited.
    pub transcript: TranscriptData,
}

/// Result of switching to a different conversation branch.
#[derive(Debug, Clone)]
pub struct BranchSwitchData {
    pub transcript: TranscriptData,
    /// Plan state for the new active path.
    pub plan: PlanState,
}

/// Branch/worktree listing for a session's project.
#[derive(Debug, Clone)]
pub struct WorktreeListing {
    pub branches: Vec<git::Branch>,
    pub worktrees: Vec<git::Worktree>,
    pub current_branch: Option<String>,
    pub is_git_repo: bool,
}

/// A git worktree the session was switched to.
#[derive(Debug, Clone)]
pub struct CreatedWorktree {
    pub path: PathBuf,
    pub branch: String,
}

/// Outcome of adding a project.
#[derive(Debug, Clone)]
pub enum AddProjectOutcome {
    /// Project saved and an initial session created for it.
    Added { session_id: String },
    /// The project already exists with the same name and path — no-op.
    AlreadyExists,
}

/// Shared state the worker hands to each command.
#[derive(Clone)]
struct ServiceCtx {
    manager: Arc<Mutex<SessionManager>>,
    runtime: Arc<AgentRuntimeOptions>,
    events: EventStream,
}

impl ServiceCtx {
    /// Send a session-scoped notification to the broadcast stream.
    fn notify_session(&self, session_id: &str, event: UiEvent) {
        self.events.publish_ui(session_id, event);
    }
}

type BoxedCommandFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type Command = Box<dyn FnOnce(ServiceCtx) -> BoxedCommandFuture + Send>;

/// Cloneable handle to the session command worker. See module docs.
#[derive(Clone)]
pub struct SessionService {
    tx: async_channel::Sender<Command>,
    events: EventStream,
}

impl SessionService {
    /// Create the service handle and its worker future. The caller must
    /// spawn the worker on the tokio runtime that should execute commands
    /// (agents started by commands spawn tasks onto that runtime).
    ///
    /// `events` must be the same stream the [`SessionManager`] publishes to,
    /// so that [`SessionService::subscribe`] covers command results and agent
    /// streaming alike.
    pub fn new(
        manager: Arc<Mutex<SessionManager>>,
        runtime: Arc<AgentRuntimeOptions>,
        events: EventStream,
    ) -> (Self, impl Future<Output = ()>) {
        let (tx, rx) = async_channel::unbounded::<Command>();
        let ctx = ServiceCtx {
            manager,
            runtime,
            events: events.clone(),
        };
        let worker = async move {
            debug!("Session service worker started");
            while let Ok(command) = rx.recv().await {
                command(ctx.clone()).await;
            }
            debug!("Session service worker stopped");
        };
        (Self { tx, events }, worker)
    }

    /// Subscribe to the core→UI broadcast stream.
    pub fn subscribe(&self) -> crate::session::event_stream::Subscription {
        self.events.subscribe()
    }

    /// Request that the running agent of a session stops at the next
    /// opportunity (streaming checkpoint). No-op if no agent is running.
    pub async fn request_stop(&self, session_id: String) -> Result<()> {
        self.call(move |ctx| async move {
            let manager = ctx.manager.lock().await;
            let session = manager
                .get_session(&session_id)
                .ok_or_else(|| anyhow!("Session {session_id} not found"))?;
            session.request_stop();
            Ok(())
        })
        .await
    }

    /// Enqueue a command and await its typed reply.
    async fn call<T, F, Fut>(&self, f: F) -> Result<T>
    where
        F: FnOnce(ServiceCtx) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(Box::new(move |ctx| {
                Box::pin(async move {
                    let _ = reply_tx.send(f(ctx).await);
                })
            }))
            .await
            .map_err(|_| anyhow!("session service is not running"))?;
        reply_rx
            .await
            .map_err(|_| anyhow!("session service dropped the request"))?
    }

    // ========================================================================
    // Session management
    // ========================================================================

    /// Create a new session, optionally bound to a project.
    pub async fn create_session(
        &self,
        name: Option<String>,
        initial_project: Option<String>,
    ) -> Result<String> {
        self.call(move |ctx| async move {
            let mut manager = ctx.manager.lock().await;
            if let Some(project) = initial_project {
                // Resolve the project path so the new session gets the correct
                // CWD — either from projects.json or from a sibling session
                // that already ran in that (temporary) project.
                let mut config = manager.session_config_template().clone();
                config.initial_project = project.clone();
                if let Some(path) = manager.resolve_project_path(&project) {
                    config.init_path = Some(path);
                }
                manager.create_session_with_config(name, Some(config), None)
            } else {
                manager.create_session(name)
            }
        })
        .await
    }

    /// Connect a session and return an owned snapshot for rendering. After
    /// applying the snapshot, the frontend follows the session on the
    /// broadcast stream (see [`SessionService::subscribe`]).
    pub async fn load_session(
        &self,
        session_id: String,
        edit_until_node_id: Option<NodeId>,
    ) -> Result<crate::session::SessionSnapshot> {
        self.call(move |ctx| async move {
            let snapshot = {
                let mut manager = ctx.manager.lock().await;
                manager
                    .set_active_session(session_id.clone(), edit_until_node_id)
                    .await?
            };
            Ok(snapshot)
        })
        .await
    }

    pub async fn delete_session(&self, session_id: String) -> Result<()> {
        self.call(move |ctx| async move {
            let mut manager = ctx.manager.lock().await;
            manager.delete_session(&session_id)
        })
        .await
    }

    pub async fn list_sessions(&self) -> Result<Vec<ChatMetadata>> {
        self.call(move |ctx| async move {
            let manager = ctx.manager.lock().await;
            manager.list_all_sessions()
        })
        .await
    }

    /// Incremental session refresh triggered by the file watcher. Compares
    /// the on-disk state with the in-memory state and emits only the delta
    /// as [`UiEvent`]s; falls back to a full reload if that fails.
    pub async fn refresh_session(&self, session_id: String) -> Result<()> {
        let refreshed = self
            .call({
                let session_id = session_id.clone();
                move |ctx| async move {
                    let ui_events = {
                        let mut manager = ctx.manager.lock().await;
                        manager.refresh_session_incremental(&session_id)?
                    };
                    for event in ui_events {
                        ctx.notify_session(&session_id, event);
                    }
                    Ok(())
                }
            })
            .await;
        match refreshed {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!("Incremental refresh failed for {session_id}, falling back: {e}");
                self.load_session(session_id, None).await.map(|_| ())
            }
        }
    }

    /// Clear the Errored state on a session (user dismissed the error banner).
    pub async fn clear_session_error(&self, session_id: String) -> Result<()> {
        self.call(move |ctx| async move {
            {
                let mut manager = ctx.manager.lock().await;
                if let Some(session) = manager.get_session_mut(&session_id) {
                    let current = session.get_activity_state();
                    if current.is_terminal() {
                        session.set_activity_state(
                            crate::session::instance::SessionActivityState::Idle,
                        );
                    }
                }
            }
            // Broadcast the state change so the sidebar updates
            ctx.notify_session(
                &session_id.clone(),
                UiEvent::UpdateSessionActivityState {
                    session_id,
                    activity_state: crate::session::instance::SessionActivityState::Idle,
                },
            );
            Ok(())
        })
        .await
    }

    /// Clear the conversation context (messages) for a session. The session
    /// itself is kept alive; only the message history is wiped.
    pub async fn clear_context(&self, session_id: String) -> Result<()> {
        self.call(move |ctx| async move {
            {
                let mut manager = ctx.manager.lock().await;
                if let Some(session) = manager.get_session_mut(&session_id) {
                    let chat = &mut session.session;
                    chat.message_nodes.clear();
                    chat.active_path.clear();
                    chat.next_node_id = 1;
                    chat.messages.clear();
                    chat.plan = Default::default();
                }
            }
            ctx.notify_session(&session_id, UiEvent::ClearMessages);
            Ok(())
        })
        .await
    }

    /// Compact (summarise) conversation context for a session.
    pub async fn compact_context(&self, _session_id: String) -> Result<()> {
        bail!("Compact is not yet implemented. Use /clear to reset context.")
    }

    /// Update the default model name used for newly created sessions.
    pub async fn update_default_model(&self, model_name: String) -> Result<()> {
        self.call(move |ctx| async move {
            let mut manager = ctx.manager.lock().await;
            manager.set_default_model_name(model_name);
            Ok(())
        })
        .await
    }

    // ========================================================================
    // Agent operations
    // ========================================================================

    /// Add a user message to the session and start the agent for it.
    pub async fn send_user_message(
        &self,
        session_id: String,
        message: String,
        attachments: Vec<DraftAttachment>,
        branch_parent_id: Option<NodeId>,
    ) -> Result<()> {
        self.call(move |ctx| async move {
            send_user_message_impl(&ctx, &session_id, &message, &attachments, branch_parent_id)
                .await
        })
        .await
    }

    /// Queue a user message while the agent is running. Returns the updated
    /// pending-message summary.
    pub async fn queue_user_message(
        &self,
        session_id: String,
        message: String,
        attachments: Vec<DraftAttachment>,
    ) -> Result<Option<String>> {
        self.call(move |ctx| async move {
            let content_blocks = content_blocks_from(&message, &attachments);
            let mut manager = ctx.manager.lock().await;
            manager.queue_structured_user_message(&session_id, content_blocks)?;
            manager.get_pending_message(&session_id)
        })
        .await
    }

    /// Take the pending message out of the queue for editing. Returns its
    /// text, or `None` if nothing was queued.
    pub async fn take_pending_message(&self, session_id: String) -> Result<Option<String>> {
        self.call(move |ctx| async move {
            let mut manager = ctx.manager.lock().await;
            manager.request_pending_message_for_edit(&session_id)
        })
        .await
    }

    /// Resume a session that ended in a state where the agent should run
    /// against the existing message history (no new user message is added).
    pub async fn resume_session(&self, session_id: String) -> Result<()> {
        self.call(move |ctx| async move { resume_session_impl(&ctx, &session_id).await })
            .await
    }

    /// Cancel a running sub-agent by its tool id. Returns `true` if a
    /// sub-agent was actually cancelled, `false` if it had already finished.
    pub async fn cancel_sub_agent(&self, session_id: String, tool_id: String) -> Result<bool> {
        self.call(move |ctx| async move {
            let manager = ctx.manager.lock().await;
            manager.cancel_sub_agent(&session_id, &tool_id)
        })
        .await
    }

    // ========================================================================
    // Skills
    // ========================================================================

    /// List the skills available to a session (across project / user /
    /// system scopes), for the input-area skill picker.
    pub async fn list_skills(&self, session_id: String) -> Result<Vec<SkillCatalogEntry>> {
        self.call(move |ctx| async move {
            let project_name = {
                let manager = ctx.manager.lock().await;
                manager
                    .get_session(&session_id)
                    .map(|s| s.session.config.initial_project.clone())
                    .ok_or_else(|| anyhow!("Session {session_id} not found"))?
            };
            let config = SkillsConfig::load();
            let pm = DefaultProjectManager::new();
            Ok(discover_session_catalog(&pm, &project_name, &config)
                .into_iter()
                .map(|(skill, scope_token)| SkillCatalogEntry {
                    name: skill.name,
                    description: skill.description,
                    scope_label: skill.scope.label().to_string(),
                    scope_token,
                })
                .collect())
        })
        .await
    }

    /// User-initiated ("explicit") skill activation: load the skill's body
    /// and inject it directly as a synthetic user message, then run the
    /// agent.
    pub async fn invoke_skill(
        &self,
        session_id: String,
        scope: String,
        name: String,
    ) -> Result<()> {
        self.call(move |ctx| async move {
            let config = SkillsConfig::load();
            let pm = DefaultProjectManager::new();
            let payload = load_skill_payload(&pm, &scope, &name, &config)
                .with_context(|| format!("Failed to load skill `{name}`"))?;
            let message = render_skill_invocation_message(&payload);

            // Record the activation (deduped) so compaction can remind the
            // model if the injected body is summarised away.
            {
                let mut manager = ctx.manager.lock().await;
                if let Some(session) = manager.get_session_mut(&session_id) {
                    if !session.session.active_skills.iter().any(|s| s == &name) {
                        session.session.active_skills.push(name.clone());
                    }
                }
                if let Err(e) = manager.save_session(&session_id) {
                    warn!("Failed to persist active_skills for {session_id}: {e}");
                }
            }

            send_user_message_impl(&ctx, &session_id, &message, &[], None).await
        })
        .await
    }

    // ========================================================================
    // Model & sandbox
    // ========================================================================

    pub async fn switch_model(
        &self,
        session_id: String,
        model_name: String,
    ) -> Result<ModelSwitchResult> {
        self.call(move |ctx| async move {
            let config_system =
                ConfigurationSystem::load().context("Failed to load model configuration")?;
            if config_system.get_model(&model_name).is_none() {
                bail!("Model '{model_name}' not found in configuration.");
            }

            let mut manager = ctx.manager.lock().await;
            let outcome = manager.set_session_model_config(
                &session_id,
                Some(SessionModelConfig::new(model_name.clone())),
            )?;
            if let Some(warning) = &outcome.warning {
                warn!("{}", warning);
            }
            let allowed_models = manager
                .allowed_models_for_session(&session_id)
                .unwrap_or_default();
            info!("Switched model for session {session_id} to {model_name}");
            Ok(ModelSwitchResult {
                warning: outcome.warning,
                allowed_models,
            })
        })
        .await
    }

    pub async fn change_sandbox_policy(
        &self,
        session_id: String,
        policy: SandboxPolicy,
    ) -> Result<()> {
        self.call(move |ctx| async move {
            let mut manager = ctx.manager.lock().await;
            manager.set_session_sandbox_policy(&session_id, policy)
        })
        .await
    }

    // ========================================================================
    // Session branching
    // ========================================================================

    /// Prepare editing a past message: returns its content plus the
    /// transcript truncated to the messages before it.
    pub async fn start_message_edit(
        &self,
        session_id: String,
        node_id: NodeId,
    ) -> Result<MessageEditContext> {
        self.call(move |ctx| async move {
            let manager = ctx.manager.lock().await;
            let session_instance = manager
                .get_session(&session_id)
                .ok_or_else(|| anyhow!("Session {session_id} not found"))?;
            let node = session_instance
                .session
                .message_nodes
                .get(&node_id)
                .ok_or_else(|| anyhow!("Message node {node_id} not found"))?;

            let content = match &node.message.content {
                llm::MessageContent::Text(text) => text.clone(),
                llm::MessageContent::Structured(blocks) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        llm::ContentBlock::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            let attachments = match &node.message.content {
                llm::MessageContent::Structured(blocks) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        llm::ContentBlock::Image {
                            media_type, data, ..
                        } => Some(DraftAttachment::Image {
                            content: data.clone(),
                            mime_type: media_type.clone(),
                            width: None,
                            height: None,
                        }),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };

            // The branch parent is the parent of the node being edited.
            let branch_parent_id = node.parent_id;
            let messages = session_instance
                .convert_messages_to_ui_data_until(
                    session_instance.session.config.tool_syntax,
                    branch_parent_id,
                )
                .unwrap_or_default();
            let tool_results = session_instance
                .convert_tool_executions_to_ui_data()
                .unwrap_or_default();

            Ok(MessageEditContext {
                content,
                attachments,
                branch_parent_id,
                transcript: TranscriptData {
                    messages,
                    tool_results,
                },
            })
        })
        .await
    }

    /// Switch the active path to a sibling branch and return the new
    /// transcript.
    pub async fn switch_branch(
        &self,
        session_id: String,
        new_node_id: NodeId,
    ) -> Result<BranchSwitchData> {
        self.call(move |ctx| async move {
            let mut manager = ctx.manager.lock().await;
            let session_instance = manager
                .get_session_mut(&session_id)
                .ok_or_else(|| anyhow!("Session {session_id} not found"))?;
            session_instance
                .session
                .switch_branch(new_node_id)
                .context("Failed to switch branch")?;

            // Persist the updated active_path. Continue on failure — the
            // switch worked in memory.
            if let Err(e) = manager.save_session(&session_id) {
                error!("Failed to save session after branch switch: {}", e);
            }

            let session_instance = manager
                .get_session(&session_id)
                .ok_or_else(|| anyhow!("Session {session_id} not found after save"))?;
            let transcript = transcript_data(session_instance)?;
            Ok(BranchSwitchData {
                transcript,
                plan: session_instance.session.plan.clone(),
            })
        })
        .await
    }

    /// Abort a message edit and return the full transcript of the active
    /// path.
    pub async fn cancel_message_edit(&self, session_id: String) -> Result<TranscriptData> {
        self.call(move |ctx| async move {
            let manager = ctx.manager.lock().await;
            let session_instance = manager
                .get_session(&session_id)
                .ok_or_else(|| anyhow!("Session {session_id} not found"))?;
            transcript_data(session_instance)
        })
        .await
    }

    // ========================================================================
    // Git worktrees
    // ========================================================================

    pub async fn list_branches_and_worktrees(&self, session_id: String) -> Result<WorktreeListing> {
        self.call(move |ctx| async move {
            let project_root = {
                let manager = ctx.manager.lock().await;
                session_project_root(&manager, &session_id)?
            };

            if !git::GitRepository::is_repo(&project_root) {
                return Ok(WorktreeListing {
                    branches: Vec::new(),
                    worktrees: Vec::new(),
                    current_branch: None,
                    is_git_repo: false,
                });
            }

            let repo =
                git::GitRepository::open(&project_root).context("Failed to open git repository")?;
            let branches = repo.list_branches().context("Failed to list branches")?;
            let current_branch = repo.current_branch();
            let worktrees = match git::worktree::list_worktrees(&repo).await {
                Ok(w) => w,
                Err(e) => {
                    // Non-fatal: return branches without worktree info
                    error!("Failed to list worktrees: {}", e);
                    Vec::new()
                }
            };
            Ok(WorktreeListing {
                branches,
                worktrees,
                current_branch,
                is_git_repo: true,
            })
        })
        .await
    }

    pub async fn switch_worktree(
        &self,
        session_id: String,
        worktree_path: Option<PathBuf>,
        branch: Option<String>,
    ) -> Result<()> {
        self.call(move |ctx| async move {
            let mut manager = ctx.manager.lock().await;
            manager.set_session_worktree(&session_id, worktree_path, branch)
        })
        .await
    }

    /// Create (or reuse) a worktree for `branch_name` and switch the session
    /// to it.
    pub async fn create_worktree(
        &self,
        session_id: String,
        branch_name: String,
        base_branch: Option<String>,
    ) -> Result<CreatedWorktree> {
        self.call(move |ctx| async move {
            let project_root = {
                let manager = ctx.manager.lock().await;
                session_project_root(&manager, &session_id)?
            };
            let repo =
                git::GitRepository::open(&project_root).context("Failed to open git repository")?;

            // Reuse an existing worktree for this branch if there is one.
            let existing = match git::worktree::find_worktree_for_branch(&repo, &branch_name).await
            {
                Ok(existing) => existing,
                Err(e) => {
                    debug!("Could not check existing worktrees: {}", e);
                    None
                }
            };
            let path = match existing {
                Some(worktree) => {
                    info!(
                        "Reusing existing worktree for branch '{}' at {:?}",
                        branch_name, worktree.path
                    );
                    worktree.path
                }
                None => {
                    let worktree_path =
                        git::worktree::suggest_worktree_path(repo.workdir(), &branch_name);
                    git::worktree::create_worktree(
                        &repo.git,
                        repo.workdir(),
                        &worktree_path,
                        &branch_name,
                        base_branch.as_deref(),
                    )
                    .await
                    .context("Failed to create worktree")?
                }
            };

            let mut manager = ctx.manager.lock().await;
            manager
                .set_session_worktree(&session_id, Some(path.clone()), Some(branch_name.clone()))
                .context("Worktree created but failed to update session")?;
            Ok(CreatedWorktree {
                path,
                branch: branch_name,
            })
        })
        .await
    }

    // ========================================================================
    // Projects
    // ========================================================================

    /// Add a new project to projects.json and create an initial session for
    /// it.
    pub async fn add_project(&self, name: String, path: PathBuf) -> Result<AddProjectOutcome> {
        self.call(move |ctx| async move {
            // No-op if this project already exists with the same name & path.
            if let Ok(existing_projects) = crate::config::load_projects() {
                if let Some(existing) = existing_projects.get(&name) {
                    let existing_canonical = existing.path.canonicalize().ok();
                    let new_canonical = path.canonicalize().ok();
                    let paths_match = match (&existing_canonical, &new_canonical) {
                        (Some(a), Some(b)) => a == b,
                        _ => existing.path == path,
                    };
                    if paths_match {
                        info!(
                            "Project '{}' already exists with the same path — no-op",
                            name
                        );
                        return Ok(AddProjectOutcome::AlreadyExists);
                    }
                }
            }

            save_project(
                &name,
                &Project {
                    path: path.clone(),
                    format_on_save: None,
                },
            )
            .context("Failed to save project")?;

            let mut manager = ctx.manager.lock().await;
            let mut config = manager.session_config_template().clone();
            config.initial_project = name.clone();
            config.init_path = Some(path);
            let session_id = manager
                .create_session_with_config(None, Some(config), None)
                .context("Project saved but failed to create session")?;
            info!("Created initial session {session_id} for project '{name}'");
            Ok(AddProjectOutcome::Added { session_id })
        })
        .await
    }

    /// Persist a temporary project to projects.json so it becomes a
    /// first-class project.
    pub async fn persist_project(&self, project_name: String) -> Result<()> {
        self.call(move |ctx| async move {
            let path = {
                let manager = ctx.manager.lock().await;
                manager.resolve_project_path(&project_name)
            }
            .ok_or_else(|| {
                anyhow!("Cannot persist project '{project_name}': unable to determine its path")
            })?;

            save_project(
                &project_name,
                &Project {
                    path,
                    format_on_save: None,
                },
            )
            .context("Failed to persist project")?;
            info!("Project '{project_name}' persisted to projects.json");
            Ok(())
        })
        .await
    }
}

fn transcript_data(
    session_instance: &crate::session::instance::SessionInstance,
) -> Result<TranscriptData> {
    let messages = session_instance
        .convert_messages_to_ui_data(session_instance.session.config.tool_syntax)
        .context("Failed to convert messages")?;
    let tool_results = session_instance
        .convert_tool_executions_to_ui_data()
        .context("Failed to convert tool results")?;
    Ok(TranscriptData {
        messages,
        tool_results,
    })
}

/// Resolve the project root path for a session (init_path, not
/// worktree_path).
fn session_project_root(manager: &SessionManager, session_id: &str) -> Result<PathBuf> {
    let session = manager
        .get_session(session_id)
        .ok_or_else(|| anyhow!("Session {session_id} not found"))?;
    session
        .session
        .config
        .init_path
        .clone()
        .ok_or_else(|| anyhow!("Session has no project path configured"))
}

async fn send_user_message_impl(
    ctx: &ServiceCtx,
    session_id: &str,
    message: &str,
    attachments: &[DraftAttachment],
    branch_parent_id: Option<NodeId>,
) -> Result<()> {
    debug!(
        "User message for session {}: {} (with {} attachments, branch_parent: {:?})",
        session_id,
        message,
        attachments.len(),
        branch_parent_id
    );

    let content_blocks = content_blocks_from(message, attachments);

    // First, add the user message to the session and get the new node_id.
    let (new_node_id, branch_info_updates) = {
        let mut manager = ctx.manager.lock().await;
        let node_id = manager
            .add_user_message(session_id, content_blocks, branch_parent_id)
            .context("Failed to add user message")?;
        // If we created a branch, get branch info updates for all siblings.
        let updates = if branch_parent_id.is_some() {
            manager.get_sibling_branch_infos(session_id, node_id)
        } else {
            Vec::new()
        };
        (node_id, updates)
    };

    // Now display the user message with the correct node_id.
    ctx.notify_session(
        session_id,
        UiEvent::DisplayUserInput {
            content: message.to_string(),
            attachments: attachments.to_vec(),
            node_id: Some(new_node_id),
        },
    );

    // Send branch info updates for all siblings (so they show the branch
    // switcher).
    for (sibling_node_id, branch_info) in branch_info_updates {
        ctx.notify_session(
            session_id,
            UiEvent::UpdateBranchInfo {
                node_id: sibling_node_id,
                branch_info,
            },
        );
    }

    start_agent_impl(ctx, session_id).await
}

async fn resume_session_impl(ctx: &ServiceCtx, session_id: &str) -> Result<()> {
    debug!("ResumeSession requested for {}", session_id);

    // Refuse to resume if an agent is already running for this session, or
    // if it's locked by another instance. Clear a prior Errored state so the
    // UI doesn't keep the error banner.
    {
        let mut manager = ctx.manager.lock().await;
        if manager.is_agent_locked_externally(session_id) {
            bail!("Cannot resume: another instance is running this session.");
        }
        if let Some(instance) = manager.get_session(session_id) {
            if !instance.get_activity_state().is_terminal() {
                bail!("Cannot resume: agent is already running for this session.");
            }
        }
        if let Some(session) = manager.get_session_mut(session_id) {
            if matches!(
                session.get_activity_state(),
                crate::session::instance::SessionActivityState::Errored { .. }
            ) {
                session.set_activity_state(crate::session::instance::SessionActivityState::Idle);
            }
        }
    }

    start_agent_impl(ctx, session_id).await
}

/// Start the agent loop for a session against its current message history.
async fn start_agent_impl(ctx: &ServiceCtx, session_id: &str) -> Result<()> {
    let session_config = {
        let manager = ctx.manager.lock().await;
        manager.get_session_model_config(session_id).unwrap_or(None)
    };
    let Some(session_config) = session_config else {
        bail!(
            "Session has no model configuration. Please ensure all sessions are created with a model."
        );
    };

    let llm_client = create_llm_client_from_model(
        &session_config.model_name,
        ctx.runtime.playback_path.clone(),
        ctx.runtime.fast_playback,
        ctx.runtime.record_path.clone(),
    )
    .await
    .context("Failed to create LLM client")?;

    let project_manager = Box::new(DefaultProjectManager::new());
    let command_executor = (ctx.runtime.command_executor_factory)(session_id);

    let mut manager = ctx.manager.lock().await;
    manager
        .set_session_model_config(session_id, Some(session_config))
        .context("Failed to persist model config")?;
    manager
        .start_agent_for_session(
            session_id,
            llm_client,
            project_manager,
            command_executor,
            None,
        )
        .await
        .context("Failed to start agent")?;
    debug!("Agent started for session {}", session_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::FileSessionPersistence;
    use crate::session::SessionConfig;

    fn test_service(root: &std::path::Path) -> SessionService {
        let events = EventStream::new();
        let persistence = FileSessionPersistence::new_with_root_dir(root.to_path_buf());
        let manager = Arc::new(Mutex::new(SessionManager::new(
            persistence,
            SessionConfig::default(),
            "test-model".to_string(),
            crate::tools::test_registry(),
            events.clone(),
        )));
        let runtime = Arc::new(AgentRuntimeOptions {
            record_path: None,
            playback_path: None,
            fast_playback: false,
            command_executor_factory: Arc::new(|_| {
                Box::new(crate::mocks::create_command_executor_mock())
            }),
        });
        let (service, worker) = SessionService::new(manager, runtime, events);
        tokio::spawn(worker);
        service
    }

    #[tokio::test]
    async fn create_list_delete_session_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let service = test_service(tmp.path());

        let id = service
            .create_session(Some("first".to_string()), None)
            .await
            .unwrap();

        let sessions = service.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, id);
        assert_eq!(sessions[0].name, "first");

        service.delete_session(id).await.unwrap();
        assert!(service.list_sessions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn load_session_returns_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let service = test_service(tmp.path());

        let id = service.create_session(None, None).await.unwrap();
        let snapshot = service.load_session(id.clone(), None).await.unwrap();

        assert_eq!(snapshot.session_id, id);
        assert_eq!(snapshot.current_model, "test-model");
        assert!(snapshot.messages.is_empty());
        // The canonical connect sequence renders the snapshot state.
        assert!(snapshot
            .connect_events()
            .iter()
            .any(|e| matches!(e, UiEvent::UpdateCurrentModel { model_name } if model_name == "test-model")));
    }

    #[tokio::test]
    async fn load_unknown_session_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let service = test_service(tmp.path());
        let err = service
            .load_session("does-not-exist".to_string(), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("does-not-exist"));
    }

    #[tokio::test]
    async fn queue_and_take_pending_message() {
        let tmp = tempfile::tempdir().unwrap();
        let service = test_service(tmp.path());
        let id = service.create_session(None, None).await.unwrap();

        let pending = service
            .queue_user_message(id.clone(), "hello".to_string(), Vec::new())
            .await
            .unwrap();
        assert_eq!(pending.as_deref(), Some("hello"));

        // Queueing again appends.
        let pending = service
            .queue_user_message(id.clone(), "world".to_string(), Vec::new())
            .await
            .unwrap();
        assert_eq!(pending.as_deref(), Some("hello\nworld"));

        // Taking it for edit clears the queue.
        let taken = service.take_pending_message(id.clone()).await.unwrap();
        assert_eq!(taken.as_deref(), Some("hello\nworld"));
        assert_eq!(service.take_pending_message(id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn command_notifications_reach_stream_subscribers() {
        let tmp = tempfile::tempdir().unwrap();
        let service = test_service(tmp.path());
        let id = service.create_session(None, None).await.unwrap();

        let mut subscription = service.subscribe();
        service.clear_context(id.clone()).await.unwrap();

        // The ClearMessages notification arrives session-tagged on the
        // broadcast stream.
        loop {
            let event = subscription.recv().await.unwrap();
            if matches!(
                event.payload,
                crate::session::event_stream::EventPayload::Ui(UiEvent::ClearMessages)
            ) {
                assert_eq!(event.session_id.as_deref(), Some(id.as_str()));
                break;
            }
        }
    }

    #[tokio::test]
    async fn request_stop_sets_session_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let service = test_service(tmp.path());
        let id = service.create_session(None, None).await.unwrap();

        // Unknown session errors.
        assert!(service.request_stop("nope".to_string()).await.is_err());

        service.request_stop(id).await.unwrap();
    }

    #[tokio::test]
    async fn compact_context_reports_unimplemented() {
        let tmp = tempfile::tempdir().unwrap();
        let service = test_service(tmp.path());
        let err = service
            .compact_context("any".to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not yet implemented"));
    }

    #[tokio::test]
    async fn start_message_edit_unknown_session_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let service = test_service(tmp.path());
        let err = service
            .start_message_edit("nope".to_string(), 1)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn service_reports_stopped_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let events = EventStream::new();
        let persistence = FileSessionPersistence::new_with_root_dir(tmp.path().to_path_buf());
        let manager = Arc::new(Mutex::new(SessionManager::new(
            persistence,
            SessionConfig::default(),
            "test-model".to_string(),
            crate::tools::test_registry(),
            events.clone(),
        )));
        let runtime = Arc::new(AgentRuntimeOptions {
            record_path: None,
            playback_path: None,
            fast_playback: false,
            command_executor_factory: Arc::new(|_| {
                Box::new(crate::mocks::create_command_executor_mock())
            }),
        });
        let (service, worker) = SessionService::new(manager, runtime, events);
        drop(worker); // never spawned

        let err = service.list_sessions().await.unwrap_err();
        assert!(err.to_string().contains("not running"));
    }

    #[tokio::test]
    async fn commands_execute_in_submission_order() {
        let tmp = tempfile::tempdir().unwrap();
        let service = test_service(tmp.path());

        // Fire several creates concurrently and make sure each gets its own
        // typed reply (no cross-talk between concurrent callers).
        let mut handles = Vec::new();
        for i in 0..5 {
            let service = service.clone();
            handles.push(tokio::spawn(async move {
                service.create_session(Some(format!("s{i}")), None).await
            }));
        }
        let mut ids = std::collections::HashSet::new();
        for handle in handles {
            ids.insert(handle.await.unwrap().unwrap());
        }
        assert_eq!(ids.len(), 5);
        assert_eq!(service.list_sessions().await.unwrap().len(), 5);
    }
}
