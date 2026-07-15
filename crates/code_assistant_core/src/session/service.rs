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

use crate::config::{save_project, DefaultProjectManager, ProjectManager};
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

/// Builds the [`ProjectManager`] an agent run (and skill discovery) resolves
/// its `project` arguments against. Injected by the wiring so a consumer can
/// supply its own project set — pal, for instance, exposes a fixed
/// `workspace`/`home` set instead of the config-file-driven default.
pub type ProjectManagerFactory = Arc<dyn Fn() -> Box<dyn ProjectManager> + Send + Sync>;

/// The default factory: the config-file-driven [`DefaultProjectManager`],
/// preserving code-assistant's own behavior when a consumer does not override
/// the project set.
pub fn default_project_manager_factory() -> ProjectManagerFactory {
    Arc::new(|| Box::new(DefaultProjectManager::new()))
}

/// Builds the LLM client for a model name when an agent run starts. `None`
/// uses the configured providers (`create_llm_client_from_model`); tests and
/// fault-injection harnesses supply scripted providers here.
pub type LlmClientFactory = Arc<dyn Fn(&str) -> Result<Box<dyn llm::LLMProvider>> + Send + Sync>;

/// Options for running agents: LLM recording/playback plus the command
/// executor and project-manager factories.
#[derive(Clone)]
pub struct AgentRuntimeOptions {
    pub record_path: Option<PathBuf>,
    pub playback_path: Option<PathBuf>,
    pub fast_playback: bool,
    /// Builds the command executor for a session id when an agent is started.
    pub command_executor_factory: CommandExecutorFactory,
    /// Builds the project manager an agent run resolves `project` arguments
    /// against. See [`default_project_manager_factory`].
    pub project_manager_factory: ProjectManagerFactory,
    /// Overrides LLM client construction per run. See [`LlmClientFactory`].
    pub llm_client_factory: Option<LlmClientFactory>,
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

    /// Interrupt (Ctrl-C) the `execute_command` terminal identified by
    /// `tool_id` in the given session — the UI's terminal-card stop button.
    /// Tries the background PTY session first, then a foreground blocking
    /// command. Returns `Ok(())` regardless of whether a match was found
    /// (the process may have already finished).
    pub async fn interrupt_terminal(&self, session_id: String, tool_id: String) -> Result<()> {
        self.call(move |ctx| async move {
            let manager = ctx.manager.lock().await;
            let session = manager
                .get_session(&session_id)
                .ok_or_else(|| anyhow!("Session {session_id} not found"))?;
            // Background (session-mode) command: signal its PtySession.
            if session.pty_sessions.interrupt_by_tool_id(&tool_id) {
                return Ok(());
            }
            // Foreground (blocking) command: set its cancel flag.
            session.terminal_interrupts.request(&tool_id);
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

    /// Create a session from a fully specified [`SessionConfig`] and an
    /// optional model override, for callers that must place a session in a
    /// specific sandbox / workdir / project rather than inheriting the
    /// deployment default — e.g. a supervised-delegation child that runs
    /// read-only, or confined to its own workdir, on its own model.
    ///
    /// Thin wrapper over [`SessionManager::create_session_with_config`]:
    /// unlike [`Self::create_session`] the caller owns the whole config. When
    /// `model` is `None` the manager's default model is used, so the session is
    /// always runnable (a session without a model configuration cannot start an
    /// agent).
    ///
    /// [`SessionConfig`]: crate::session::SessionConfig
    pub async fn create_session_with_config(
        &self,
        name: Option<String>,
        config: crate::session::SessionConfig,
        model: Option<String>,
    ) -> Result<String> {
        self.call(move |ctx| async move {
            let mut manager = ctx.manager.lock().await;
            let model_name = model.unwrap_or_else(|| manager.default_model_name().to_string());
            let model_config = Some(SessionModelConfig::new(model_name));
            manager.create_session_with_config(name, Some(config), model_config)
        })
        .await
    }

    /// The session-config template new sessions are minted from (sandbox
    /// policy, tool syntax, permission tier, project root). Callers building a
    /// [`SessionConfig`] for [`Self::create_session_with_config`] start from
    /// this so a specialised session (e.g. a delegation child) overrides only
    /// what it must and otherwise matches the deployment's normal sessions.
    ///
    /// [`SessionConfig`]: crate::session::SessionConfig
    pub async fn session_config_template(&self) -> Result<crate::session::SessionConfig> {
        self.call(move |ctx| async move {
            let manager = ctx.manager.lock().await;
            Ok(manager.session_config_template().clone())
        })
        .await
    }

    /// Terminate the running agent on a session (best-effort): aborts the
    /// in-flight turn and releases the cross-process agent lock. A session that
    /// is already idle is unaffected. Used to reclaim compute when a
    /// system-initiated turn — such as an isolated supervised-delegation child —
    /// is cancelled from the outside.
    pub async fn terminate_agent(&self, session_id: String) -> Result<()> {
        self.call(move |ctx| async move {
            let mut manager = ctx.manager.lock().await;
            manager.terminate_session_agent(&session_id);
            Ok(())
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
            send_user_message_impl(
                &ctx,
                &session_id,
                &message,
                &attachments,
                branch_parent_id,
                None,
                None,
            )
            .await
        })
        .await
    }

    /// Like [`Self::send_user_message`], but the agent run is restricted to
    /// the tools carrying `tool_scope`'s capability tag. Per-run only; the
    /// session's next turn uses its normal scope again. For system-initiated
    /// turns such as a memory-only session wrap-up.
    pub async fn send_user_message_scoped(
        &self,
        session_id: String,
        message: String,
        attachments: Vec<DraftAttachment>,
        tool_scope: crate::tools::core::ToolScope,
    ) -> Result<()> {
        self.call(move |ctx| async move {
            send_user_message_impl(
                &ctx,
                &session_id,
                &message,
                &attachments,
                None,
                Some(tool_scope),
                None,
            )
            .await
        })
        .await
    }

    /// Add a user message, starting the agent when the session is idle or
    /// queueing into the running turn — decided inside the service, where
    /// the actual activity state is visible. Channel adapters use this
    /// instead of a send-vs-queue decision on their side, which races agent
    /// start/completion (e.g. when dispatching a burst of messages that
    /// arrived while the process was down).
    pub async fn send_or_queue_user_message(
        &self,
        session_id: String,
        message: String,
        attachments: Vec<DraftAttachment>,
    ) -> Result<()> {
        self.call(move |ctx| async move {
            send_or_queue_user_message_impl(&ctx, &session_id, &message, &attachments).await
        })
        .await
    }

    /// Start a turn only when the session is idle, decided inside the service
    /// actor. Unlike [`Self::send_or_queue_user_message`], a busy session is
    /// neither appended to nor queued and returns `false`. This is the atomic
    /// claim used by autonomous controllers that must never mistake another
    /// turn's completion for their own.
    pub async fn try_send_user_message_if_idle(
        &self,
        session_id: String,
        message: String,
        attachments: Vec<DraftAttachment>,
    ) -> Result<bool> {
        self.call(move |ctx| async move {
            try_send_user_message_if_idle_impl(&ctx, &session_id, &message, &attachments).await
        })
        .await
    }

    /// The typed sibling of [`Self::try_send_user_message_if_idle`]: start a
    /// turn only when the session is idle (atomically, inside the actor) and
    /// return a [`crate::session::TurnHandle`] identifying exactly the turn
    /// that was started. The handle resolves once with a bounded
    /// [`crate::session::TurnOutcome`] — final narration, tool and resource
    /// evidence, usage, and whether user input was absorbed — collected
    /// synchronously at the publisher, so the caller never infers "its" turn
    /// from the lossy broadcast stream.
    ///
    /// This is the dispatch seam for autonomous controllers (goal passes,
    /// delegated child runs, work-graph workers) and for tests/automation
    /// that need an exact turn result.
    pub async fn start_turn_if_idle(
        &self,
        session_id: String,
        request: crate::session::TurnRequest,
    ) -> Result<crate::session::TurnDispatch> {
        let service = self.clone();
        self.call(move |ctx| async move {
            start_turn_if_idle_impl(&ctx, service, session_id, request).await
        })
        .await
    }

    /// Whether the session is currently running a turn, decided from the live
    /// in-memory activity state inside the service actor (authoritative). This
    /// is the same ground truth [`Self::try_send_user_message_if_idle`] gates
    /// on, exposed as a read-only probe.
    ///
    /// A frontend's event-derived activity mirror is a *lossy* substitute: a
    /// lagging broadcast receiver can drop the running transition and leave the
    /// mirror frozen at a stale terminal state. A caller that gates an
    /// autonomous action on "is the session free" (a controller pass) must read
    /// this rather than a mirror, or it will act on a session the atomic send
    /// then refuses. Loads the session on demand, like the send paths.
    pub async fn is_session_busy(&self, session_id: String) -> Result<bool> {
        self.call(move |ctx| async move {
            let mut manager = ctx.manager.lock().await;
            manager.ensure_session_loaded(&session_id)?;
            let instance = manager
                .get_session(&session_id)
                .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;
            Ok(!instance.get_activity_state().is_terminal())
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

    /// Deliver a fired wakeup (see [`crate::session::wakeup`]): inject
    /// `message` into the session and make sure a turn runs for it — started
    /// immediately when the session is idle, queued as the pending message
    /// when an agent is currently running. A session that no longer exists
    /// swallows the wakeup silently.
    pub async fn inject_wakeup(&self, session_id: String, message: String) -> Result<()> {
        self.call(move |ctx| async move { inject_wakeup_impl(&ctx, &session_id, &message).await })
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
            let pm = (ctx.runtime.project_manager_factory)();
            Ok(
                discover_session_catalog(pm.as_ref(), &project_name, &config)
                    .into_iter()
                    .map(|(skill, scope_token)| SkillCatalogEntry {
                        name: skill.name,
                        description: skill.description,
                        scope_label: skill.scope.label().to_string(),
                        scope_token,
                    })
                    .collect(),
            )
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
            let pm = (ctx.runtime.project_manager_factory)();
            let payload = load_skill_payload(pm.as_ref(), &scope, &name, &config)
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

            send_user_message_impl(&ctx, &session_id, &message, &[], None, None, None).await
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

            let outcome = {
                let mut manager = ctx.manager.lock().await;
                manager.set_session_model_config(
                    &session_id,
                    Some(SessionModelConfig::new(model_name.clone())),
                )?
            };
            if let Some(warning) = &outcome.warning {
                warn!("{}", warning);
            }
            let allowed_models = {
                let manager = ctx.manager.lock().await;
                manager
                    .allowed_models_for_session(&session_id)
                    .unwrap_or_default()
            };
            info!("Switched model for session {session_id} to {model_name}");

            // Fan out the change so every view of this session updates; the
            // warning stays caller-only (it belongs to the interaction).
            ctx.notify_session(&session_id, UiEvent::UpdateCurrentModel { model_name });
            ctx.notify_session(
                &session_id,
                UiEvent::UpdateAllowedModels {
                    models: allowed_models.clone(),
                },
            );

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
            {
                let mut manager = ctx.manager.lock().await;
                manager.set_session_sandbox_policy(&session_id, policy.clone())?;
            }
            // Fan out the change so every view of this session updates.
            ctx.notify_session(&session_id, UiEvent::UpdateSandboxPolicy { policy });
            Ok(())
        })
        .await
    }

    /// Change when the agent asks for permission before running tools.
    /// Takes effect on the next agent run.
    pub async fn change_permission_tier(
        &self,
        session_id: String,
        tier: tools_core::PermissionTier,
    ) -> Result<()> {
        self.call(move |ctx| async move {
            {
                let mut manager = ctx.manager.lock().await;
                manager.set_session_permission_tier(&session_id, tier)?;
            }
            // Fan out the change so every view of this session updates.
            ctx.notify_session(&session_id, UiEvent::UpdatePermissionTier { tier });
            Ok(())
        })
        .await
    }

    /// Answer a pending tool permission request
    /// ([`UiEvent::RequestToolPermission`]). Unknown request ids are ignored
    /// (the request may have been settled by another view or a stop).
    pub async fn respond_permission(
        &self,
        session_id: String,
        request_id: String,
        decision: tools_core::PermissionDecision,
    ) -> Result<()> {
        self.call(move |ctx| async move {
            let manager = ctx.manager.lock().await;
            manager.resolve_permission_request(&session_id, &request_id, decision)?;
            Ok(())
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
    tool_scope_override: Option<crate::tools::core::ToolScope>,
    turn_recorder: Option<Arc<crate::session::turn::TurnRecorder>>,
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
        // Headless dispatch (channel adapters, schedulers) reaches sessions
        // no frontend has opened since the restart — load on demand.
        manager.ensure_session_loaded(session_id)?;
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

    start_agent_impl(ctx, session_id, tool_scope_override, turn_recorder).await
}

/// Shared by [`SessionService::send_or_queue_user_message`] and the wakeup
/// path: decide send-vs-queue by the session's actual activity state.
async fn send_or_queue_user_message_impl(
    ctx: &ServiceCtx,
    session_id: &str,
    message: &str,
    attachments: &[DraftAttachment],
) -> Result<()> {
    let running = {
        let mut manager = ctx.manager.lock().await;
        // Headless dispatch (channel adapters, schedulers) reaches sessions
        // no frontend has opened since the restart — load on demand.
        manager.ensure_session_loaded(session_id)?;
        let instance = manager
            .get_session(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;
        !instance.get_activity_state().is_terminal()
    };

    if running {
        let content_blocks = content_blocks_from(message, attachments);
        let mut manager = ctx.manager.lock().await;
        manager.queue_structured_user_message(session_id, content_blocks)?;
        if let Some(summary) = manager.get_pending_message(session_id)? {
            ctx.notify_session(
                session_id,
                UiEvent::UpdatePendingMessage {
                    message: Some(summary),
                },
            );
        }
        return Ok(());
    }

    send_user_message_impl(ctx, session_id, message, attachments, None, None, None).await
}

async fn try_send_user_message_if_idle_impl(
    ctx: &ServiceCtx,
    session_id: &str,
    message: &str,
    attachments: &[DraftAttachment],
) -> Result<bool> {
    let idle = {
        let mut manager = ctx.manager.lock().await;
        manager.ensure_session_loaded(session_id)?;
        let instance = manager
            .get_session(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;
        instance.get_activity_state().is_terminal()
    };
    if !idle {
        return Ok(false);
    }

    send_user_message_impl(ctx, session_id, message, attachments, None, None, None).await?;
    Ok(true)
}

async fn start_turn_if_idle_impl(
    ctx: &ServiceCtx,
    service: SessionService,
    session_id: String,
    request: crate::session::TurnRequest,
) -> Result<crate::session::TurnDispatch> {
    use crate::session::turn::TurnRecorder;
    use crate::session::{TurnDispatch, TurnHandle};

    // The idle check and the usage baseline for the outcome's token delta
    // come from the same lock scope; the actor serializes commands, so no
    // other dispatch can slip in before the send below.
    let baseline_usage = {
        let mut manager = ctx.manager.lock().await;
        manager.ensure_session_loaded(&session_id)?;
        let instance = manager
            .get_session(&session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;
        if !instance.get_activity_state().is_terminal() {
            return Ok(TurnDispatch::Busy);
        }
        instance.calculate_total_usage()
    };

    let (recorder, parts) = TurnRecorder::arm(baseline_usage);
    send_user_message_impl(
        ctx,
        &session_id,
        &request.message,
        &request.attachments,
        None,
        request.tool_scope,
        Some(recorder),
    )
    .await?;
    Ok(TurnDispatch::Started(TurnHandle::new(
        session_id,
        parts.turn_id,
        service,
        parts.outcome,
    )))
}

async fn inject_wakeup_impl(ctx: &ServiceCtx, session_id: &str, message: &str) -> Result<()> {
    // Session deleted since the wakeup was armed — drop silently.
    if ctx.manager.lock().await.get_session(session_id).is_none() {
        debug!("Wakeup for unknown session {session_id} dropped");
        return Ok(());
    }
    send_or_queue_user_message_impl(ctx, session_id, message, &[]).await
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

    start_agent_impl(ctx, session_id, None, None).await
}

/// Start the agent loop for a session against its current message history.
/// The model config an agent run will use: the session's own — unless it
/// records a model unknown to the current configuration (models.json
/// changed since the session last ran), then the default model. Without
/// the fallback such a session is permanently undispatchable, which
/// headless channels (no model picker) cannot recover from. The caller
/// persists the result, so the session heals.
fn runnable_model_config(
    session_config: SessionModelConfig,
    default_model_name: &str,
    model_exists: impl Fn(&str) -> bool,
) -> SessionModelConfig {
    if model_exists(&session_config.model_name) {
        return session_config;
    }
    warn!(
        "Session records unknown model '{}'; falling back to '{default_model_name}'",
        session_config.model_name
    );
    SessionModelConfig::new(default_model_name.to_string())
}

async fn start_agent_impl(
    ctx: &ServiceCtx,
    session_id: &str,
    tool_scope_override: Option<crate::tools::core::ToolScope>,
    turn_recorder: Option<Arc<crate::session::turn::TurnRecorder>>,
) -> Result<()> {
    let (session_config, default_model_name) = {
        let manager = ctx.manager.lock().await;
        (
            manager.get_session_model_config(session_id).unwrap_or(None),
            manager.default_model_name().to_string(),
        )
    };
    let Some(mut session_config) = session_config else {
        bail!(
            "Session has no model configuration. Please ensure all sessions are created with a model."
        );
    };

    // Validation is fail-soft: without a loadable configuration the client
    // construction below reports the error.
    if let Ok(config_system) = ConfigurationSystem::load() {
        session_config = runnable_model_config(session_config, &default_model_name, |model| {
            config_system.get_model(model).is_some()
        });
    }

    let llm_client = match &ctx.runtime.llm_client_factory {
        Some(factory) => factory(&session_config.model_name)
            .context("Failed to create LLM client from injected factory")?,
        None => create_llm_client_from_model(
            &session_config.model_name,
            ctx.runtime.playback_path.clone(),
            ctx.runtime.fast_playback,
            ctx.runtime.record_path.clone(),
        )
        .await
        .context("Failed to create LLM client")?,
    };

    let project_manager = (ctx.runtime.project_manager_factory)();
    let command_executor = (ctx.runtime.command_executor_factory)(session_id);

    let mut manager = ctx.manager.lock().await;
    manager
        .set_session_model_config(session_id, Some(session_config))
        .context("Failed to persist model config")?;
    // Permission prompts travel over the event stream to whatever frontend
    // views this session; the tier decides whether the agent ever asks.
    let permission_handler = manager.permission_mediator(session_id).ok();
    manager
        .start_agent_for_session(
            session_id,
            llm_client,
            project_manager,
            command_executor,
            permission_handler,
            tool_scope_override,
            turn_recorder,
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

    fn test_service_with_manager(
        root: &std::path::Path,
    ) -> (SessionService, Arc<Mutex<SessionManager>>) {
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
            project_manager_factory: default_project_manager_factory(),
            llm_client_factory: None,
        });
        let (service, worker) = SessionService::new(manager.clone(), runtime, events);
        tokio::spawn(worker);
        (service, manager)
    }

    fn test_service(root: &std::path::Path) -> SessionService {
        test_service_with_manager(root).0
    }

    /// A provider that streams its scripted text through the callback (like
    /// a real provider) and returns it as the response — enough to drive a
    /// complete agent turn without any network.
    struct StreamingScriptedProvider {
        text: String,
    }

    #[async_trait::async_trait]
    impl llm::LLMProvider for StreamingScriptedProvider {
        async fn send_message(
            &mut self,
            _request: llm::LLMRequest,
            streaming_callback: Option<&llm::StreamingCallback>,
        ) -> Result<llm::LLMResponse> {
            if let Some(callback) = streaming_callback {
                callback(&llm::StreamingChunk::Text(self.text.clone()))?;
                callback(&llm::StreamingChunk::StreamingComplete)?;
            }
            Ok(llm::LLMResponse {
                content: vec![llm::ContentBlock::new_text(&self.text)],
                usage: llm::Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
                rate_limit_info: None,
            })
        }
    }

    /// Service whose agent runs use the injected LLM factory instead of the
    /// configured providers.
    fn test_service_with_llm(
        root: &std::path::Path,
        factory: LlmClientFactory,
    ) -> (SessionService, Arc<Mutex<SessionManager>>) {
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
            project_manager_factory: default_project_manager_factory(),
            llm_client_factory: Some(factory),
        });
        let (service, worker) = SessionService::new(manager.clone(), runtime, events);
        tokio::spawn(worker);
        (service, manager)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn start_turn_if_idle_resolves_the_exact_outcome() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _) = test_service_with_llm(
            tmp.path(),
            Arc::new(|_model| {
                Ok(Box::new(StreamingScriptedProvider {
                    text: "Considered it carefully; done.".to_string(),
                }))
            }),
        );
        let id = service.create_session(None, None).await.unwrap();

        let dispatch = service
            .start_turn_if_idle(
                id.clone(),
                crate::session::TurnRequest::text("please do the thing"),
            )
            .await
            .unwrap();
        let handle = match dispatch {
            crate::session::TurnDispatch::Started(handle) => handle,
            crate::session::TurnDispatch::Busy => panic!("fresh session reported busy"),
        };
        assert_eq!(handle.session_id(), id);

        let outcome = handle.wait().await.unwrap();
        assert_eq!(outcome.status, crate::session::TurnStatus::Completed);
        assert_eq!(outcome.final_response, "Considered it carefully; done.");
        assert_eq!(outcome.usage.llm_requests, 1);
        assert!(!outcome.user_preempted);
        // The token delta comes from the persisted-state notifications.
        let tokens = outcome.usage.tokens.expect("usage recorded");
        assert_eq!(tokens.output_tokens, 5);

        // The turn is over: the session is idle again and a second turn gets
        // a distinct turn id.
        match service
            .start_turn_if_idle(id.clone(), crate::session::TurnRequest::text("again"))
            .await
            .unwrap()
        {
            crate::session::TurnDispatch::Started(second) => {
                assert_ne!(second.turn_id(), outcome.turn_id);
                let _ = second.wait().await.unwrap();
            }
            crate::session::TurnDispatch::Busy => panic!("session still busy after outcome"),
        }
    }

    #[tokio::test]
    async fn start_turn_if_idle_refuses_a_busy_session_without_queueing() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, manager) = test_service_with_manager(tmp.path());
        let id = service.create_session(None, None).await.unwrap();
        {
            let mut manager = manager.lock().await;
            manager
                .get_session_mut(&id)
                .unwrap()
                .set_activity_state(crate::session::instance::SessionActivityState::AgentRunning);
        }

        match service
            .start_turn_if_idle(id.clone(), crate::session::TurnRequest::text("nope"))
            .await
            .unwrap()
        {
            crate::session::TurnDispatch::Busy => {}
            crate::session::TurnDispatch::Started(_) => panic!("dispatched into a busy session"),
        }
        // Nothing was appended or queued.
        let snapshot = service.load_session(id.clone(), None).await.unwrap();
        assert!(snapshot.messages.is_empty());
        assert_eq!(service.take_pending_message(id).await.unwrap(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_turn_still_resolves_with_a_failed_outcome() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, _) = test_service_with_llm(
            tmp.path(),
            Arc::new(|_model| {
                Ok(Box::new(crate::mocks::MockLLMProvider::new(vec![Err(
                    anyhow::anyhow!("model exploded"),
                )])))
            }),
        );
        let id = service.create_session(None, None).await.unwrap();

        let handle = match service
            .start_turn_if_idle(id, crate::session::TurnRequest::text("try"))
            .await
            .unwrap()
        {
            crate::session::TurnDispatch::Started(handle) => handle,
            crate::session::TurnDispatch::Busy => panic!("fresh session reported busy"),
        };
        match handle.wait().await.unwrap().status {
            crate::session::TurnStatus::Failed { error } => {
                assert!(
                    error.contains("model exploded"),
                    "unexpected error: {error}"
                )
            }
            status => panic!("expected Failed, got {status:?}"),
        }
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
    async fn inject_wakeup_into_unknown_session_is_dropped_silently() {
        let tmp = tempfile::tempdir().unwrap();
        let service = test_service(tmp.path());
        service
            .inject_wakeup("gone".to_string(), "[scheduled wakeup] x".to_string())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn inject_wakeup_queues_while_agent_is_running() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, manager) = test_service_with_manager(tmp.path());
        let id = service.create_session(None, None).await.unwrap();

        {
            let mut manager = manager.lock().await;
            manager
                .get_session_mut(&id)
                .unwrap()
                .set_activity_state(crate::session::instance::SessionActivityState::AgentRunning);
        }

        service
            .inject_wakeup(id.clone(), "[scheduled wakeup] check".to_string())
            .await
            .unwrap();

        let pending = service.take_pending_message(id).await.unwrap();
        assert_eq!(pending.as_deref(), Some("[scheduled wakeup] check"));
    }

    #[tokio::test]
    async fn inject_wakeup_into_idle_session_adds_the_message() {
        let tmp = tempfile::tempdir().unwrap();
        let service = test_service(tmp.path());
        let id = service.create_session(None, None).await.unwrap();

        // Starting the turn fails in this harness (no real LLM client), but
        // the injected message must land in the history either way — exactly
        // like a user message sent to an idle session.
        let _ = service
            .inject_wakeup(id.clone(), "[scheduled wakeup] check".to_string())
            .await;

        let snapshot = service.load_session(id, None).await.unwrap();
        assert_eq!(snapshot.messages.len(), 1);
    }

    #[tokio::test]
    async fn send_or_queue_queues_while_agent_is_running() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, manager) = test_service_with_manager(tmp.path());
        let id = service.create_session(None, None).await.unwrap();

        {
            let mut manager = manager.lock().await;
            manager
                .get_session_mut(&id)
                .unwrap()
                .set_activity_state(crate::session::instance::SessionActivityState::AgentRunning);
        }

        service
            .send_or_queue_user_message(id.clone(), "while busy".to_string(), Vec::new())
            .await
            .unwrap();

        let pending = service.take_pending_message(id).await.unwrap();
        assert_eq!(pending.as_deref(), Some("while busy"));
    }

    #[tokio::test]
    async fn try_send_if_idle_refuses_busy_session_without_queueing_or_appending() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, manager) = test_service_with_manager(tmp.path());
        let id = service.create_session(None, None).await.unwrap();
        {
            let mut manager = manager.lock().await;
            manager
                .get_session_mut(&id)
                .unwrap()
                .set_activity_state(crate::session::instance::SessionActivityState::AgentRunning);
        }

        let started = service
            .try_send_user_message_if_idle(id.clone(), "controller turn".to_string(), Vec::new())
            .await
            .unwrap();

        assert!(!started);
        assert_eq!(
            service.take_pending_message(id.clone()).await.unwrap(),
            None
        );
        let snapshot = service.load_session(id, None).await.unwrap();
        assert!(snapshot.messages.is_empty());
    }

    #[tokio::test]
    async fn is_session_busy_reads_the_authoritative_activity_state() {
        let tmp = tempfile::tempdir().unwrap();
        let (service, manager) = test_service_with_manager(tmp.path());
        let id = service.create_session(None, None).await.unwrap();

        // A freshly created session is idle.
        assert!(!service.is_session_busy(id.clone()).await.unwrap());

        // Flip the live state to running; the probe reflects it even though no
        // activity event was published to any mirror.
        {
            let mut manager = manager.lock().await;
            manager
                .get_session_mut(&id)
                .unwrap()
                .set_activity_state(crate::session::instance::SessionActivityState::AgentRunning);
        }
        assert!(service.is_session_busy(id.clone()).await.unwrap());
    }

    #[tokio::test]
    async fn send_or_queue_sends_to_an_idle_session() {
        let tmp = tempfile::tempdir().unwrap();
        let service = test_service(tmp.path());
        let id = service.create_session(None, None).await.unwrap();

        // Starting the turn fails in this harness (no real LLM client), but
        // the message must land in the history — exactly like a plain send.
        let _ = service
            .send_or_queue_user_message(id.clone(), "while idle".to_string(), Vec::new())
            .await;

        let snapshot = service.load_session(id.clone(), None).await.unwrap();
        assert_eq!(snapshot.messages.len(), 1);
        assert_eq!(
            service.take_pending_message(id).await.unwrap(),
            None,
            "nothing may sit in the queue when the session was idle"
        );
    }

    /// The restart case for headless dispatch: the session exists on disk
    /// but no frontend has loaded it into the manager. A message arriving
    /// for it (channel adapter, scheduler) must load it on demand instead
    /// of failing with "Session not found".
    #[tokio::test]
    async fn dispatch_loads_a_persisted_session_after_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let id = {
            let service = test_service(tmp.path());
            let id = service.create_session(None, None).await.unwrap();
            // Give the session a message so the next startup's
            // empty-session cleanup does not remove it.
            let _ = service
                .send_or_queue_user_message(id.clone(), "first".to_string(), Vec::new())
                .await;
            id
        };

        // A fresh service over the same store — nothing is resident.
        let service = test_service(tmp.path());
        let _ = service
            .send_or_queue_user_message(id.clone(), "after restart".to_string(), Vec::new())
            .await;
        let snapshot = service.load_session(id, None).await.unwrap();
        assert_eq!(snapshot.messages.len(), 2);

        // Same restart case for the plain send path (the scheduler's).
        let tmp = tempfile::tempdir().unwrap();
        let id = {
            let service = test_service(tmp.path());
            let id = service.create_session(None, None).await.unwrap();
            let _ = service
                .send_user_message(id.clone(), "first".to_string(), Vec::new(), None)
                .await;
            id
        };
        let service = test_service(tmp.path());
        let _ = service
            .send_user_message(id.clone(), "after restart".to_string(), Vec::new(), None)
            .await;
        let snapshot = service.load_session(id, None).await.unwrap();
        assert_eq!(snapshot.messages.len(), 2);
    }

    #[test]
    fn runnable_model_config_falls_back_when_the_model_vanished() {
        let kept = runnable_model_config(
            SessionModelConfig::new("known".to_string()),
            "default",
            |model| model == "known",
        );
        assert_eq!(kept.model_name, "known");

        let replaced = runnable_model_config(
            SessionModelConfig::new("vanished".to_string()),
            "default",
            |_| false,
        );
        assert_eq!(replaced.model_name, "default");
    }

    #[tokio::test]
    async fn send_or_queue_to_unknown_session_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let service = test_service(tmp.path());
        // Unlike a wakeup (fire-and-forget), a channel adapter must learn
        // that its dispatch target is gone.
        assert!(service
            .send_or_queue_user_message("gone".to_string(), "hi".to_string(), Vec::new())
            .await
            .is_err());
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
            project_manager_factory: default_project_manager_factory(),
            llm_client_factory: None,
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

    #[tokio::test]
    async fn change_permission_tier_persists_and_notifies() {
        use tools_core::PermissionTier;

        let tmp = tempfile::tempdir().unwrap();
        let service = test_service(tmp.path());
        let id = service.create_session(None, None).await.unwrap();

        let mut subscription = service.subscribe();
        service
            .change_permission_tier(id.clone(), PermissionTier::AllTools)
            .await
            .unwrap();

        // The change is fanned out to stream subscribers...
        loop {
            let event = subscription.recv().await.unwrap();
            if let crate::session::event_stream::EventPayload::Ui(UiEvent::UpdatePermissionTier {
                tier,
            }) = event.payload
            {
                assert_eq!(tier, PermissionTier::AllTools);
                assert_eq!(event.session_id.as_deref(), Some(id.as_str()));
                break;
            }
        }

        // ...and lands in the snapshot (persisted config).
        let snapshot = service.load_session(id.clone(), None).await.unwrap();
        assert_eq!(snapshot.permission_tier, PermissionTier::AllTools);
        assert!(snapshot.connect_events().iter().any(|e| matches!(
            e,
            UiEvent::UpdatePermissionTier {
                tier: PermissionTier::AllTools
            }
        )));

        // Unknown session errors.
        assert!(service
            .change_permission_tier("nope".to_string(), PermissionTier::BypassAll)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn respond_permission_resolves_pending_request() {
        use tools_core::permissions::PermissionDecision;

        let tmp = tempfile::tempdir().unwrap();
        let (service, manager) = test_service_with_manager(tmp.path());
        let id = service.create_session(None, None).await.unwrap();

        // Build the same mediator the agent would get and fire a request.
        let mediator = manager.lock().await.permission_mediator(&id).unwrap();

        let mut subscription = service.subscribe();
        let request_task = tokio::spawn(async move {
            let params = serde_json::json!({"paths": ["a.txt"]});
            mediator
                .request_permission(tools_core::PermissionRequest {
                    tool_id: Some("tool-1"),
                    tool_name: "delete_files",
                    reason: tools_core::PermissionRequestReason::ToolInvocation { params: &params },
                })
                .await
        });

        // The prompt arrives on the stream; answer it via the service.
        let request_id = loop {
            let event = subscription.recv().await.unwrap();
            if let crate::session::event_stream::EventPayload::Ui(
                UiEvent::RequestToolPermission { request },
            ) = event.payload
            {
                assert_eq!(request.tool_name, "delete_files");
                break request.request_id;
            }
        };

        service
            .respond_permission(
                id.clone(),
                request_id.clone(),
                PermissionDecision::GrantedOnce,
            )
            .await
            .unwrap();

        assert_eq!(
            request_task.await.unwrap().unwrap(),
            PermissionDecision::GrantedOnce
        );

        // Every view is told the request settled.
        loop {
            let event = subscription.recv().await.unwrap();
            if let crate::session::event_stream::EventPayload::Ui(
                UiEvent::ToolPermissionRequestResolved { request_id: rid },
            ) = event.payload
            {
                assert_eq!(rid, request_id);
                break;
            }
        }
    }
}
