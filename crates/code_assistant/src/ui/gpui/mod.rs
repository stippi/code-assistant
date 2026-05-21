pub mod shared;

// Re-exports for backward compatibility during incremental migration
pub use shared::assets;
pub use shared::context_indicator;
pub use shared::file_icons;
pub use shared::image;
pub use shared::plan_banner;
pub use shared::settings;
pub use shared::theme;
pub use shared::ui_state;

mod backend;
pub mod blocks;
pub mod elements;
mod event_loop;
pub mod input;
pub mod main_screen;
pub mod messages;
pub mod project_sidebar;
mod root;
pub mod settings_screen;
pub mod terminal;
pub mod tool_cards;
mod user_interface_impl;

// Re-exports for backward compatibility during migration
pub use terminal::executor as terminal_executor;
pub use terminal::pool as terminal_pool;

use crate::persistence::{ChatMetadata, DraftStorage};
use crate::types::PlanState;
use crate::ui::UiEvent;
use assets::Assets;
use async_channel;
use elements::MessageContainer;
use gpui::{
    actions, px, App, AppContext, AsyncApp, Entity, Global, KeyBinding, Menu, MenuItem, Point,
    SharedString,
};
use gpui_component::Root;
pub use messages::MessagesView;
pub use root::RootView;
use sandbox::SandboxPolicy;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, trace, warn};

actions!(
    code_assistant,
    [Quit, CloseWindow, InsertLineBreak, CancelAgent]
);

// Global UI event sender for chat components
#[derive(Clone)]
pub struct UiEventSender(pub async_channel::Sender<UiEvent>);

impl Global for UiEventSender {}

/// Global wrapper for persisted UI settings so entities can read/write them.
#[derive(Clone)]
pub struct UiSettingsGlobal(pub settings::UiSettings);

impl Global for UiSettingsGlobal {}

// Re-export backend types for compatibility
pub use crate::ui::backend::{BackendEvent, BackendResponse};

/// Snapshot of worktree/branch data for the active session, kept in `Gpui`
/// so that `RootView::render()` can push it into the `WorktreeSelector`.
#[derive(Debug, Clone, PartialEq)]
pub struct WorktreeData {
    pub worktrees: Vec<git::Worktree>,
    pub current_worktree_path: Option<std::path::PathBuf>,
    pub is_git_repo: bool,
}

// Our main UI struct that implements the UserInterface trait
#[derive(Clone)]
pub struct Gpui {
    message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
    plan_state: Arc<Mutex<Option<PlanState>>>,
    event_sender: Arc<Mutex<async_channel::Sender<UiEvent>>>,
    event_receiver: Arc<Mutex<async_channel::Receiver<UiEvent>>>,
    event_task: Arc<Mutex<Option<gpui::Task<()>>>>,
    session_event_task: Arc<Mutex<Option<gpui::Task<()>>>>,
    current_request_id: Arc<Mutex<u64>>,
    // Unified backend communication
    backend_event_sender: Arc<Mutex<Option<async_channel::Sender<BackendEvent>>>>,
    backend_response_receiver: Arc<Mutex<Option<async_channel::Receiver<BackendResponse>>>>,

    // Current chat state
    current_session_id: Arc<Mutex<Option<String>>>,
    chat_sessions: Arc<Mutex<Vec<ChatMetadata>>>,
    current_session_activity_state:
        Arc<Mutex<Option<crate::session::instance::SessionActivityState>>>,
    // Track which session has requested streaming to stop
    session_stop_requests: Arc<Mutex<std::collections::HashSet<String>>>,

    // UI components
    project_sidebar: Arc<Mutex<Option<Entity<project_sidebar::SessionSidebar>>>>,
    messages_view: Arc<Mutex<Option<Entity<MessagesView>>>>,

    // Draft storage system
    draft_storage: Arc<DraftStorage>,
    session_drafts: Arc<Mutex<HashMap<String, String>>>,

    // Error state management
    current_error: Arc<Mutex<Option<String>>>,
    // Transient status notification (auto-dismisses after a few seconds)
    transient_status: Arc<Mutex<Option<String>>>,

    // Current model selection
    current_model: Arc<Mutex<Option<String>>>,

    // Current sandbox selection
    current_sandbox_policy: Arc<Mutex<Option<SandboxPolicy>>>,

    // Current worktree state (branches + worktrees listing from backend)
    current_worktree_data: Arc<Mutex<Option<WorktreeData>>>,

    // Last usage from the active session's most recent assistant message.
    // Stored separately from chat_sessions so it cannot be overwritten by
    // stale metadata loaded from disk (via UpdateChatList / ListSessions).
    current_session_last_usage: Arc<Mutex<Option<llm::Usage>>>,

    // Pending message edit state (for branching)
    pending_edit: Arc<Mutex<Option<PendingEdit>>>,

    // Debounce task for persisting per-session UI state files
    ui_state_save_task: Arc<Mutex<Option<gpui::Task<()>>>>,

    /// Project names that exist in projects.json (i.e. first-class projects).
    /// Used by the sidebar to decide whether to show a "persist" icon.
    persisted_projects: Arc<Mutex<std::collections::HashSet<String>>>,

    /// Incremented each time config files (providers.json / models.json) change on disk.
    /// Components compare their locally cached generation with this to know when to reload.
    config_generation: Arc<std::sync::atomic::AtomicU64>,
}

/// State for a pending message edit (for branching)
#[derive(Clone, Debug)]
pub struct PendingEdit {
    pub content: String,
    pub attachments: Vec<crate::persistence::DraftAttachment>,
    pub branch_parent_id: Option<crate::persistence::NodeId>,
}

fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        // Line break keybindings - ENTER with any modifier inserts a line break
        KeyBinding::new("shift-enter", InsertLineBreak, None),
        KeyBinding::new("ctrl-enter", InsertLineBreak, None),
        KeyBinding::new("alt-enter", InsertLineBreak, None),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-enter", InsertLineBreak, None),
        // Cancel agent with Esc key
        KeyBinding::new("escape", CancelAgent, None),
    ]);

    cx.on_action(|_: &Quit, cx: &mut App| {
        cx.quit();
    });

    use gpui_component::input::{Copy, Cut, Paste, Redo, Undo};
    cx.set_menus(vec![
        Menu {
            name: "GPUI App".into(),
            items: vec![MenuItem::action("Quit", Quit)],
            disabled: false,
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Undo", Undo, gpui::OsAction::Undo),
                MenuItem::os_action("Redo", Redo, gpui::OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", Cut, gpui::OsAction::Cut),
                MenuItem::os_action("Copy", Copy, gpui::OsAction::Copy),
                MenuItem::os_action("Paste", Paste, gpui::OsAction::Paste),
            ],
            disabled: false,
        },
        Menu {
            name: "Window".into(),
            items: vec![],
            disabled: false,
        },
    ]);
    cx.activate(true);
}

// Implement Global trait for Gpui
impl Global for Gpui {}

impl Gpui {
    // Helper methods for entity updates to reduce boilerplate

    /// Update the last message container in the queue
    fn update_last_message<F>(&self, cx: &mut gpui::AsyncApp, f: F)
    where
        F: FnOnce(&mut MessageContainer, &mut gpui::Context<MessageContainer>),
    {
        let last = self.message_queue.lock().unwrap().last().cloned();
        if let Some(last) = last {
            cx.update_entity(&last, f);
        }
    }

    /// Update all message containers in the queue
    fn update_all_messages<F>(&self, cx: &mut gpui::AsyncApp, f: F)
    where
        F: Fn(&mut MessageContainer, &mut gpui::Context<MessageContainer>) + Clone,
    {
        let containers = self.message_queue.lock().unwrap().clone();
        for message_container in &containers {
            cx.update_entity(message_container, f.clone());
        }
    }

    /// Update the project sidebar entity
    fn update_project_sidebar<F>(&self, cx: &mut gpui::AsyncApp, f: F)
    where
        F: FnOnce(
            &mut project_sidebar::SessionSidebar,
            &mut gpui::Context<project_sidebar::SessionSidebar>,
        ),
    {
        let project_sidebar_entity = self.project_sidebar.lock().unwrap().clone();
        if let Some(project_sidebar_entity) = project_sidebar_entity.as_ref() {
            cx.update_entity(project_sidebar_entity, f);
        }
    }

    /// Update the messages view entity
    fn update_messages_view<F>(&self, cx: &mut gpui::AsyncApp, f: F)
    where
        F: FnOnce(&mut MessagesView, &mut gpui::Context<MessagesView>),
    {
        let messages_view_entity = self.messages_view.lock().unwrap().clone();
        if let Some(messages_view_entity) = messages_view_entity.as_ref() {
            cx.update_entity(messages_view_entity, f);
        }
    }

    /// Update a specific message container
    fn update_container<F>(
        &self,
        container: &Entity<MessageContainer>,
        cx: &mut gpui::AsyncApp,
        f: F,
    ) where
        F: FnOnce(&mut MessageContainer, &mut gpui::Context<MessageContainer>),
    {
        cx.update_entity(container, f);
    }

    /// Notify the MessagesView that items were appended to the message_queue.
    /// This splices the new items into the ListState (preserving cached heights
    /// of existing items) and triggers auto-scroll if following tail.
    fn notify_messages_appended(&self, old_len: usize, cx: &mut gpui::AsyncApp) {
        let new_len = self.message_queue.lock().unwrap().len();
        if new_len != old_len {
            self.update_messages_view(cx, |view, cx| {
                view.messages_spliced(old_len, new_len, cx);
                cx.notify();
            });
        }
    }

    /// Notify the MessagesView that the message_queue was fully reset (cleared + reloaded).
    /// This resets the ListState, discarding all cached heights.
    fn notify_messages_reset(&self, cx: &mut gpui::AsyncApp) {
        let new_len = self.message_queue.lock().unwrap().len();
        self.update_messages_view(cx, |view, cx| {
            view.messages_reset(new_len, cx);
            cx.notify();
        });
    }

    /// Keep the list scrolled to the bottom if the user is following the tail.
    /// Called after streaming content is appended to the last message, which
    /// changes the height of the last list item without changing the item count.
    fn auto_scroll_if_following(&self, cx: &mut gpui::AsyncApp) {
        self.update_messages_view(cx, |view, cx| {
            if view.follow_tail {
                view.scroll_to_bottom();
            }
            // Always notify so the list re-renders visible items that gained
            // new blocks (e.g. thinking blocks created inside an existing
            // MessageContainer).
            cx.notify();
        });
    }

    /// Remove empty containers from the message queue and sync the ListState.
    /// Called after cancellation/rollback removes blocks from containers.
    fn remove_empty_containers(&self, cx: &mut gpui::AsyncApp) {
        let mut queue = self.message_queue.lock().unwrap();
        let old_len = queue.len();
        queue.retain(|container| cx.update_entity(container, |c, _cx| !c.is_empty()));
        let new_len = queue.len();
        drop(queue);

        if new_len != old_len {
            // Full reset since items may have been removed from arbitrary positions
            self.update_messages_view(cx, |view, cx| {
                view.messages_reset(new_len, cx);
                cx.notify();
            });
        }
    }

    /// Clear all UI state associated with the current session.
    ///
    /// This resets session-scoped fields (current session id, messages, error,
    /// plan, model, etc.) so that the UI shows the "no session" state.
    ///
    /// **Note:** This does NOT update the `MessagesView` entity because that
    /// requires a context (`AsyncApp` or `Context<T>`) which differs between
    /// call sites. Callers must separately reset the messages view.
    fn clear_current_session_state(&self) {
        *self.current_session_id.lock().unwrap() = None;
        self.message_queue.lock().unwrap().clear();
        *self.current_session_activity_state.lock().unwrap() = None;
        *self.current_error.lock().unwrap() = None;
        *self.plan_state.lock().unwrap() = None;
        *self.current_model.lock().unwrap() = None;
        *self.current_sandbox_policy.lock().unwrap() = None;
        *self.current_worktree_data.lock().unwrap() = None;
        *self.current_session_last_usage.lock().unwrap() = None;
    }

    pub fn new() -> Self {
        let message_queue = Arc::new(Mutex::new(Vec::new()));
        let plan_state = Arc::new(Mutex::new(None));
        let event_task = Arc::new(Mutex::new(None::<gpui::Task<()>>));
        let session_event_task = Arc::new(Mutex::new(None::<gpui::Task<()>>));
        let current_request_id = Arc::new(Mutex::new(0));

        // Initialize tool block renderer registry
        {
            use tool_cards::{InlineToolRenderer, ToolBlockRendererRegistry};
            let mut tbr_registry = ToolBlockRendererRegistry::new();
            tbr_registry.register(Arc::new(InlineToolRenderer::new()));
            tbr_registry.register(Arc::new(tool_cards::terminal_card::TerminalCardRenderer));
            tbr_registry.register(Arc::new(tool_cards::diff_card::DiffCardRenderer));
            tbr_registry.register(Arc::new(tool_cards::sub_agent_card::SubAgentCardRenderer));
            tbr_registry.register(Arc::new(tool_cards::code_card::CodeCardRenderer));
            ToolBlockRendererRegistry::set_global(Arc::new(tbr_registry));
        }

        // Initialize the per-session UI state store (same directory as session files)
        {
            let sessions_dir = dirs::data_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("code-assistant")
                .join("sessions");
            ui_state::UiStateStore::init_global(sessions_dir);
        }

        // Create a channel to send and receive UiEvents
        let (tx, rx) = async_channel::unbounded::<UiEvent>();
        let event_sender = Arc::new(Mutex::new(tx));
        let event_receiver = Arc::new(Mutex::new(rx));

        // Initialize draft storage (using default config directory)
        let draft_storage = Arc::new(
            DraftStorage::new(
                dirs::config_dir()
                    .unwrap_or_else(|| std::env::current_dir().unwrap())
                    .join("code-assistant"),
            )
            .unwrap_or_else(|e| {
                warn!("Failed to initialize draft storage: {}, using fallback", e);
                DraftStorage::new(std::env::temp_dir().join("code-assistant-drafts"))
                    .expect("Failed to create fallback draft storage")
            }),
        );

        Self {
            message_queue,
            plan_state,
            event_sender,
            event_receiver,
            event_task,
            session_event_task,
            current_request_id,
            backend_event_sender: Arc::new(Mutex::new(None)),
            backend_response_receiver: Arc::new(Mutex::new(None)),

            current_session_id: Arc::new(Mutex::new(None)),
            chat_sessions: Arc::new(Mutex::new(Vec::new())),
            current_session_activity_state: Arc::new(Mutex::new(None)),
            session_stop_requests: Arc::new(Mutex::new(std::collections::HashSet::new())),

            project_sidebar: Arc::new(Mutex::new(None)),
            messages_view: Arc::new(Mutex::new(None)),

            // Draft storage system
            draft_storage,
            session_drafts: Arc::new(Mutex::new(HashMap::new())),

            // Error state management
            current_error: Arc::new(Mutex::new(None)),
            transient_status: Arc::new(Mutex::new(None)),

            // Current model selection
            current_model: Arc::new(Mutex::new(None)),
            // Current sandbox selection
            current_sandbox_policy: Arc::new(Mutex::new(None)),

            // Pending message edit state
            pending_edit: Arc::new(Mutex::new(None)),

            // Current worktree state
            current_worktree_data: Arc::new(Mutex::new(None)),

            // Current session last usage
            current_session_last_usage: Arc::new(Mutex::new(None)),

            // Debounce task for UI state persistence
            ui_state_save_task: Arc::new(Mutex::new(None)),

            // Load the set of persisted project names from projects.json
            persisted_projects: Arc::new(Mutex::new(
                crate::config::load_projects()
                    .unwrap_or_default()
                    .keys()
                    .cloned()
                    .collect(),
            )),

            config_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    // Run the application
    pub fn run_app(&self) {
        let message_queue = self.message_queue.clone();
        let gpui_clone = self.clone();

        // Initialize app with assets
        let app = gpui_platform::application().with_assets(Assets {});

        app.run(move |cx| {
            // Register our Gpui instance as a global
            cx.set_global(gpui_clone.clone());

            // Register UI event sender as global for chat components
            cx.set_global(UiEventSender(
                gpui_clone.event_sender.lock().unwrap().clone(),
            ));

            // Setup window close listener
            cx.bind_keys([gpui::KeyBinding::new("cmd-w", CloseWindow, None)]);
            cx.on_window_closed(|cx, _window_id| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            // Load persisted UI settings
            let ui_settings = settings::UiSettings::load();
            let saved_theme_mode = match ui_settings.theme_mode {
                settings::ThemeModeSetting::Light => Some(gpui_component::theme::ThemeMode::Light),
                settings::ThemeModeSetting::Dark => Some(gpui_component::theme::ThemeMode::Dark),
            };

            // Initialize file icons
            file_icons::init(cx);

            // Initialize gpui-component modules
            gpui_component::init(cx);
            // Apply our custom theme colors (restoring saved mode)
            theme::init_themes(cx, saved_theme_mode);

            // Restore saved font scale
            {
                let scaled = gpui::px(16.0 * ui_settings.ui_scale);
                cx.global_mut::<gpui_component::theme::Theme>().font_size = scaled;
            }

            // Store settings as a GPUI global so entities can access/update them
            cx.set_global(UiSettingsGlobal(ui_settings.clone()));

            init(cx);

            // Spawn task to receive UiEvents
            let rx = gpui_clone.event_receiver.lock().unwrap().clone();
            let async_gpui_clone = gpui_clone.clone();
            debug!("Starting UI event processing task");
            let task = cx.spawn(async move |cx: &mut AsyncApp| {
                debug!("UI event processing task is running");

                // Process bursts of events in small batches, then cooperatively
                // yield back to the GPUI executor so paint/layout work is not
                // starved by a long stream of tiny updates.
                const UI_EVENT_BATCH_SIZE: usize = 32;

                loop {
                    trace!("Waiting for UI event...");
                    let result = rx.recv().await;
                    match result {
                        Ok(received_event) => {
                            trace!("UI event processing: Received event: {:?}", received_event);
                            async_gpui_clone.process_ui_event_async(received_event, cx);

                            let mut processed_in_batch = 1;
                            while processed_in_batch < UI_EVENT_BATCH_SIZE {
                                match rx.try_recv() {
                                    Ok(received_event) => {
                                        trace!(
                                            "UI event processing: Received batched event: {:?}",
                                            received_event
                                        );
                                        async_gpui_clone.process_ui_event_async(received_event, cx);
                                        processed_in_batch += 1;
                                    }
                                    Err(async_channel::TryRecvError::Empty) => break,
                                    Err(async_channel::TryRecvError::Closed) => return,
                                }
                            }

                            cx.background_executor()
                                .timer(std::time::Duration::from_millis(1))
                                .await;
                        }
                        Err(err) => {
                            warn!("Receive error: {}", err);
                            break;
                        }
                    }
                }
            });

            // Store the task in our Gpui instance
            {
                let mut task_guard = gpui_clone.event_task.lock().unwrap();
                *task_guard = Some(task);
            }

            // Spawn task to handle chat management responses from agent
            let chat_gpui_clone = gpui_clone.clone();
            let chat_response_task = cx.spawn(async move |cx: &mut AsyncApp| {
                // Wait a bit for the communication channels to be set up.
                // NOTE: Use GPUI-native timer, not tokio::time::sleep, because
                // this runs on the GPUI foreground executor, not a tokio runtime.
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;

                loop {
                    // Check if we have a response receiver
                    let receiver_opt = chat_gpui_clone
                        .backend_response_receiver
                        .lock()
                        .unwrap()
                        .clone();
                    if let Some(receiver) = receiver_opt {
                        match receiver.recv().await {
                            Ok(response) => {
                                chat_gpui_clone.handle_backend_response(response, cx);
                            }
                            Err(_) => {
                                // Channel closed, break the loop
                                break;
                            }
                        }
                    } else {
                        // No receiver yet, wait and try again
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(100))
                            .await;
                    }
                }
            });

            // Store the chat response task as well
            {
                let mut task_guard = gpui_clone.session_event_task.lock().unwrap();
                *task_guard = Some(chat_response_task);
            }

            // Register the GPUI terminal worker so that
            // GpuiTerminalCommandExecutor can create PTY terminals.
            cx.spawn(async move |cx: &mut AsyncApp| {
                terminal_executor::register_gpui_terminal_worker(cx);
            })
            .detach();

            // Create window – restore saved bounds or fall back to centered default.
            let bounds = ui_settings
                .window_bounds
                .as_ref()
                .filter(|b| b.is_valid())
                .map(|b| b.to_gpui_bounds())
                .unwrap_or_else(|| {
                    gpui::Bounds::centered(None, gpui::size(gpui::px(1100.0), gpui::px(700.0)), cx)
                });
            // Open window with titlebar
            let window = cx
                .open_window(
                    gpui::WindowOptions {
                        window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                        titlebar: Some(gpui::TitlebarOptions {
                            title: Some(gpui::SharedString::from("Code Assistant")),
                            #[cfg(target_os = "macos")]
                            appears_transparent: true,
                            #[cfg(not(target_os = "macos"))]
                            appears_transparent: false,
                            traffic_light_position: Some(Point {
                                x: px(16.),
                                y: px(16.),
                            }),
                        }),
                        ..Default::default()
                    },
                    |window, cx| {
                        // Create MessagesView
                        let activity_state = gpui_clone.current_session_activity_state.clone();
                        let messages_view = cx
                            .new(|cx| MessagesView::new(message_queue.clone(), activity_state, cx));

                        // Store MessagesView reference in Gpui
                        *gpui_clone.messages_view.lock().unwrap() = Some(messages_view.clone());

                        // Create SessionSidebar and store it in Gpui
                        let project_sidebar = cx.new(project_sidebar::SessionSidebar::new);
                        *gpui_clone.project_sidebar.lock().unwrap() = Some(project_sidebar.clone());

                        // Create RootView
                        let root_view = cx.new(|cx| {
                            RootView::new(messages_view, project_sidebar.clone(), window, cx)
                        });

                        // Wrap in Root component
                        cx.new(|cx| Root::new(root_view, window, cx))
                    },
                )
                .expect("failed to open window");

            // Focus the TextInput if window was created successfully
            window
                .update(cx, |_root, window, cx| {
                    window.activate_window();
                    window.set_window_title(&SharedString::from("Code Assistant"));
                    // Get the MessageView from the Root
                    if let Some(_view) = window.root::<gpui_component::Root>().and_then(|root| root)
                    {
                        // Activate window
                        cx.activate(true);
                    }
                })
                .expect("failed to update window");
        });
    }

    /// Setup unified backend communication channels
    /// Returns channels for backend thread to receive events and send responses
    pub fn setup_backend_communication(
        &self,
    ) -> (
        async_channel::Receiver<BackendEvent>,
        async_channel::Sender<BackendResponse>,
    ) {
        let (event_tx, event_rx) = async_channel::unbounded::<BackendEvent>();
        let (response_tx, response_rx) = async_channel::unbounded::<BackendResponse>();

        // Store channels for UI use
        *self.backend_event_sender.lock().unwrap() = Some(event_tx);
        *self.backend_response_receiver.lock().unwrap() = Some(response_rx);

        // Return the backend ends
        (event_rx, response_tx)
    }

    // Helper to add an event to the queue
    fn push_event(&self, event: UiEvent) {
        let sender = self.event_sender.lock().unwrap().clone();
        // Non-blocking send
        if let Err(err) = sender.try_send(event) {
            warn!("Failed to send event via channel: {}", err);
        }
    }

    // Get current chat state for UI components
    pub fn get_chat_sessions(&self) -> Vec<ChatMetadata> {
        self.chat_sessions.lock().unwrap().clone()
    }

    pub fn get_current_session_id(&self) -> Option<String> {
        self.current_session_id.lock().unwrap().clone()
    }

    /// Returns a clone of the shared current-session-id mutex.
    ///
    /// Used by the filesystem watcher to know which session is being viewed.
    pub fn current_session_id_ref(&self) -> Arc<Mutex<Option<String>>> {
        self.current_session_id.clone()
    }

    /// Returns a clone of the UI event sender channel.
    ///
    /// Used by the filesystem watcher to inject events into the UI loop.
    pub fn event_sender(&self) -> async_channel::Sender<UiEvent> {
        self.event_sender.lock().unwrap().clone()
    }

    /// Current config generation counter. Incremented each time providers.json
    /// or models.json change on disk. Components compare this to their cached
    /// value to decide when to reload.
    pub fn config_generation(&self) -> u64 {
        self.config_generation
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn get_current_error(&self) -> Option<String> {
        self.current_error.lock().unwrap().clone()
    }

    pub fn get_transient_status(&self) -> Option<String> {
        self.transient_status.lock().unwrap().clone()
    }

    pub fn get_current_model(&self) -> Option<String> {
        self.current_model.lock().unwrap().clone()
    }

    pub fn get_plan_state(&self) -> Option<PlanState> {
        self.plan_state.lock().unwrap().clone()
    }

    pub fn get_current_sandbox_policy(&self) -> Option<SandboxPolicy> {
        self.current_sandbox_policy.lock().unwrap().clone()
    }

    pub fn get_current_worktree_data(&self) -> Option<WorktreeData> {
        self.current_worktree_data.lock().unwrap().clone()
    }

    pub fn get_current_session_last_usage(&self) -> Option<llm::Usage> {
        self.current_session_last_usage.lock().unwrap().clone()
    }

    /// Get and clear pending edit (used by RootView to pick up edit state)
    pub fn take_pending_edit(&self) -> Option<PendingEdit> {
        self.pending_edit.lock().unwrap().take()
    }

    /// Set pending edit state
    pub fn set_pending_edit(&self, edit: PendingEdit) {
        *self.pending_edit.lock().unwrap() = Some(edit);
    }

    // Extended draft management methods with attachments
    pub fn save_draft_for_session(
        &self,
        session_id: &str,
        content: &str,
        attachments: &[crate::persistence::DraftAttachment],
    ) {
        // Update in-memory cache
        {
            let mut drafts = self.session_drafts.lock().unwrap();
            if content.is_empty() && attachments.is_empty() {
                drafts.remove(session_id);
            } else {
                drafts.insert(session_id.to_string(), content.to_string());
            }
        }

        // Save to disk (non-blocking) with full draft structure
        let draft_storage = self.draft_storage.clone();
        let session_id_owned = session_id.to_string();
        let content_owned = content.to_string();
        let attachments_owned = attachments.to_vec();
        let session_drafts = self.session_drafts.clone();

        tokio::spawn(async move {
            // For empty content and no attachments, always try to delete (idempotent)
            if content_owned.is_empty() && attachments_owned.is_empty() {
                if let Err(e) =
                    draft_storage.save_draft(&session_id_owned, &content_owned, &attachments_owned)
                {
                    warn!(
                        "Failed to delete draft for session {}: {}",
                        session_id_owned, e
                    );
                }
                return;
            }

            // For non-empty content or attachments, check cache right before disk write
            let should_save = {
                let drafts = session_drafts.lock().unwrap();
                let exists_in_cache = drafts.contains_key(&session_id_owned);
                let current_content = drafts.get(&session_id_owned);

                // Only save if draft still exists in cache AND content matches exactly
                exists_in_cache && current_content == Some(&content_owned)
            };

            if should_save || !attachments_owned.is_empty() {
                if let Err(e) =
                    draft_storage.save_draft(&session_id_owned, &content_owned, &attachments_owned)
                {
                    warn!(
                        "Failed to save draft with attachments for session {}: {}",
                        session_id_owned, e
                    );
                }
            }
        });
    }

    pub fn load_draft_for_session(
        &self,
        session_id: &str,
    ) -> Option<(String, Vec<crate::persistence::DraftAttachment>)> {
        // First check in-memory cache for text
        let cached_text = {
            let drafts = self.session_drafts.lock().unwrap();
            drafts.get(session_id).cloned()
        };

        // Load from disk for full draft structure
        match self.draft_storage.load_draft(session_id) {
            Ok(Some((draft_text, attachments))) => {
                // Cache the loaded draft text
                {
                    let mut drafts = self.session_drafts.lock().unwrap();
                    drafts.insert(session_id.to_string(), draft_text.clone());
                }
                Some((draft_text, attachments))
            }
            Ok(None) => {
                // Check if we have cached text without attachments
                cached_text.map(|text| (text, Vec::new()))
            }
            Err(e) => {
                warn!(
                    "Failed to load draft with attachments for session {}: {}",
                    session_id, e
                );
                // Fallback to cached text if available
                cached_text.map(|text| (text, Vec::new()))
            }
        }
    }

    pub fn clear_draft_for_session(&self, session_id: &str) {
        // Remove from in-memory cache FIRST
        {
            let mut drafts = self.session_drafts.lock().unwrap();
            drafts.remove(session_id);
        }

        // Clear from disk synchronously to ensure it happens before any racing save operations
        if let Err(e) = self.draft_storage.clear_draft(session_id) {
            warn!("Failed to clear draft for session {}: {}", session_id, e);
        }
    }
}
