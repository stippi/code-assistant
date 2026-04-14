pub mod assets;
pub mod attachment;
pub mod auto_scroll;
pub mod branch_switcher;
pub mod chat_sidebar;
mod context_indicator;
pub mod diff_card_renderer;
pub mod elements;
pub mod file_icons;
pub mod image;
pub mod input_area;
mod messages;
pub mod model_selector;
pub mod new_project_dialog;
mod plan_banner;
mod root;
pub mod sandbox_selector;
pub mod sub_agent_card_renderer;
pub mod terminal_card_renderer;

pub mod settings;
pub mod terminal_executor;
pub mod terminal_pool;
pub mod theme;
pub mod tool_block_renderers;
pub mod ui_state;
pub mod worktree_selector;

use crate::persistence::{ChatMetadata, DraftStorage};
use crate::types::PlanState;
use crate::ui::gpui::elements::MessageRole;
use crate::ui::{async_trait, DisplayFragment, UIError, UiEvent, UserInterface};
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
use tracing::{debug, error, info, trace, warn};

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
    chat_sidebar: Arc<Mutex<Option<Entity<chat_sidebar::ChatSidebar>>>>,
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
        },
        Menu {
            name: "Window".into(),
            items: vec![],
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
            if let Err(err) = cx.update_entity(&last, f) {
                warn!("Failed to update last message container: {err}");
                self.prune_dropped_message_containers(cx);
            }
        }
    }

    /// Update all message containers in the queue
    fn update_all_messages<F>(&self, cx: &mut gpui::AsyncApp, f: F)
    where
        F: Fn(&mut MessageContainer, &mut gpui::Context<MessageContainer>) + Clone,
    {
        let containers = self.message_queue.lock().unwrap().clone();
        let mut had_error = false;
        for message_container in &containers {
            if let Err(err) = cx.update_entity(message_container, f.clone()) {
                warn!("Failed to update message container: {err}");
                had_error = true;
            }
        }
        if had_error {
            self.prune_dropped_message_containers(cx);
        }
    }

    /// Remove message container handles that no longer point to live entities.
    fn prune_dropped_message_containers(&self, cx: &mut gpui::AsyncApp) {
        let mut queue = self.message_queue.lock().unwrap();
        let before = queue.len();
        queue.retain(|container| cx.read_entity(container, |_, _| ()).is_ok());
        let after = queue.len();
        if before != after {
            warn!(
                "Pruned {} dropped message container(s) from queue",
                before.saturating_sub(after)
            );
            drop(queue);
            self.notify_messages_reset(cx);
        }
    }

    /// Update the chat sidebar entity
    fn update_chat_sidebar<F>(&self, cx: &mut gpui::AsyncApp, f: F)
    where
        F: FnOnce(&mut chat_sidebar::ChatSidebar, &mut gpui::Context<chat_sidebar::ChatSidebar>),
    {
        let chat_sidebar_entity = self.chat_sidebar.lock().unwrap().clone();
        if let Some(chat_sidebar_entity) = chat_sidebar_entity.as_ref() {
            if let Err(err) = cx.update_entity(chat_sidebar_entity, f) {
                warn!("Failed to update chat sidebar: {err}");
                *self.chat_sidebar.lock().unwrap() = None;
            }
        }
    }

    /// Update the messages view entity
    fn update_messages_view<F>(&self, cx: &mut gpui::AsyncApp, f: F)
    where
        F: FnOnce(&mut MessagesView, &mut gpui::Context<MessagesView>),
    {
        let messages_view_entity = self.messages_view.lock().unwrap().clone();
        if let Some(messages_view_entity) = messages_view_entity.as_ref() {
            if let Err(err) = cx.update_entity(messages_view_entity, f) {
                warn!("Failed to update messages view: {err}");
                *self.messages_view.lock().unwrap() = None;
            }
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
        if let Err(err) = cx.update_entity(container, f) {
            warn!("Failed to update message container: {err}");
            self.prune_dropped_message_containers(cx);
        }
    }

    /// Notify the MessagesView that items were appended to the message_queue.
    /// This splices the new items into the ListState (preserving cached heights
    /// of existing items) and triggers auto-scroll if following tail.
    fn notify_messages_appended(&self, old_len: usize, cx: &mut gpui::AsyncApp) {
        let new_len = self.message_queue.lock().unwrap().len();
        if new_len != old_len {
            self.update_messages_view(cx, |view, cx| {
                view.messages_spliced(old_len, new_len);
                cx.notify();
            });
        }
    }

    /// Notify the MessagesView that the message_queue was fully reset (cleared + reloaded).
    /// This resets the ListState, discarding all cached heights.
    fn notify_messages_reset(&self, cx: &mut gpui::AsyncApp) {
        let new_len = self.message_queue.lock().unwrap().len();
        self.update_messages_view(cx, |view, cx| {
            view.messages_reset(new_len);
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
                cx.notify(); // Trigger render so animation task can be spawned
            }
        });
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
            use tool_block_renderers::{InlineToolRenderer, ToolBlockRendererRegistry};
            let mut tbr_registry = ToolBlockRendererRegistry::new();
            tbr_registry.register(Arc::new(InlineToolRenderer::new()));
            tbr_registry.register(Arc::new(terminal_card_renderer::TerminalCardRenderer));
            tbr_registry.register(Arc::new(diff_card_renderer::DiffCardRenderer));
            tbr_registry.register(Arc::new(sub_agent_card_renderer::SubAgentCardRenderer));
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

            chat_sidebar: Arc::new(Mutex::new(None)),
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
        }
    }

    // Run the application
    pub fn run_app(&self) {
        let message_queue = self.message_queue.clone();
        let gpui_clone = self.clone();

        // Initialize app with assets
        let app = gpui::Application::new().with_assets(Assets {});

        app.run(move |cx| {
            // Register our Gpui instance as a global
            cx.set_global(gpui_clone.clone());

            // Register UI event sender as global for chat components
            cx.set_global(UiEventSender(
                gpui_clone.event_sender.lock().unwrap().clone(),
            ));

            // Setup window close listener
            cx.bind_keys([gpui::KeyBinding::new("cmd-w", CloseWindow, None)]);
            cx.on_window_closed(|cx| {
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

                            gpui::Timer::after(std::time::Duration::from_millis(1)).await;
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

                        // Create ChatSidebar and store it in Gpui
                        let chat_sidebar = cx.new(chat_sidebar::ChatSidebar::new);
                        *gpui_clone.chat_sidebar.lock().unwrap() = Some(chat_sidebar.clone());

                        // Create RootView
                        let root_view = cx.new(|cx| {
                            RootView::new(messages_view, chat_sidebar.clone(), window, cx)
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

    fn process_ui_event_async(&self, event: UiEvent, cx: &mut gpui::AsyncApp) {
        match event {
            UiEvent::DisplayUserInput {
                content,
                attachments,
                node_id,
            } => {
                let old_len;
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    old_len = queue.len();
                    let sid = self.current_session_id.lock().unwrap().clone();
                    let result = cx.new(|cx| {
                        let new_message = MessageContainer::with_role(MessageRole::User, cx);
                        new_message.set_session_id(sid);

                        // Set node_id for edit button support
                        new_message.set_node_id(node_id);

                        // Add text content if not empty
                        if !content.is_empty() {
                            new_message.add_text_block(&content, cx);
                        }

                        // Add attachments
                        for attachment in attachments {
                            match attachment {
                                crate::persistence::DraftAttachment::Image {
                                    content,
                                    mime_type,
                                    ..
                                } => {
                                    new_message.add_image_block(&mime_type, &content, cx);
                                }
                                crate::persistence::DraftAttachment::Text { content } => {
                                    new_message.add_text_block(&content, cx);
                                }
                                crate::persistence::DraftAttachment::File {
                                    content,
                                    filename,
                                    ..
                                } => {
                                    let file_text = format!("File: {filename}\n{content}");
                                    new_message.add_text_block(&file_text, cx);
                                }
                            }
                        }

                        new_message
                    });
                    if let Ok(new_message) = result {
                        queue.push(new_message);
                    } else {
                        warn!("Failed to create message entity");
                    }
                }

                // Sync ListState and reset pending message
                self.notify_messages_appended(old_len, cx);
                self.update_messages_view(cx, |messages_view, _cx| {
                    messages_view.update_pending_message(None);
                });
            }
            UiEvent::DisplayCompactionSummary { summary } => {
                let old_len;
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    old_len = queue.len();
                    let sid = self.current_session_id.lock().unwrap().clone();
                    let result = cx.new(|cx| {
                        let message = MessageContainer::with_role(MessageRole::User, cx);
                        message.set_session_id(sid);
                        message.add_compaction_divider(summary.clone(), cx);
                        message
                    });
                    if let Ok(new_message) = result {
                        queue.push(new_message);
                    } else {
                        warn!("Failed to create compaction summary message");
                    }
                }
                self.notify_messages_appended(old_len, cx);
            }

            UiEvent::AppendToTextBlock { content } => {
                // Since StreamingStarted ensures last container is Assistant, we can safely append
                self.update_last_message(cx, |message, cx| {
                    message.add_or_append_to_text_block(&content, cx)
                });
                self.auto_scroll_if_following(cx);
            }
            UiEvent::AppendToThinkingBlock { content } => {
                // Since StreamingStarted ensures last container is Assistant, we can safely append
                self.update_last_message(cx, |message, cx| {
                    message.add_or_append_to_thinking_block(&content, cx)
                });
                self.auto_scroll_if_following(cx);
            }
            UiEvent::StartTool { name, id } => {
                // Since StreamingStarted ensures last container is Assistant, we can safely add tool
                self.update_last_message(cx, |message, cx| {
                    message.add_tool_use_block(&name, &id, cx);
                });
                self.auto_scroll_if_following(cx);
            }

            UiEvent::UpdateToolParameter {
                tool_id,
                name,
                value,
                replace,
            } => {
                if replace {
                    warn!(
                        "GPUI event: replace tool parameter for tool_id='{}', param='{}', value_len={}",
                        tool_id,
                        name,
                        value.len()
                    );
                    self.update_all_messages(cx, |message, cx| {
                        message.replace_tool_parameter(&tool_id, &name, &value, cx);
                    });
                } else {
                    self.update_last_message(cx, |message, cx| {
                        message.add_or_update_tool_parameter(&tool_id, &name, &value, cx);
                    });
                }
                self.auto_scroll_if_following(cx);
            }
            UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
                output,
                duration_seconds,
            } => {
                self.update_all_messages(cx, |message_container, cx| {
                    message_container.update_tool_status(
                        &tool_id,
                        status,
                        message.clone(),
                        output.clone(),
                        duration_seconds,
                        cx,
                    );
                });
                self.auto_scroll_if_following(cx);
            }

            UiEvent::EndTool { id } => {
                self.update_all_messages(cx, |message_container, cx| {
                    message_container.end_tool_use(&id, cx);
                });
                self.auto_scroll_if_following(cx);
            }
            UiEvent::HiddenToolCompleted => {
                // Mark that a hidden tool completed - message container handles paragraph breaks
                self.update_last_message(cx, |message, cx| {
                    message.mark_hidden_tool_completed(cx);
                });
                self.auto_scroll_if_following(cx);
            }

            UiEvent::UpdatePlan { plan } => {
                if let Ok(mut plan_guard) = self.plan_state.lock() {
                    *plan_guard = Some(plan);
                }
                cx.refresh().expect("Failed to refresh windows");
            }
            UiEvent::SetMessages {
                messages,
                session_id,
                tool_results,
            } => {
                // Update current session ID if provided
                if let Some(ref session_id) = session_id {
                    *self.current_session_id.lock().unwrap() = Some(session_id.clone());
                    // Reset activity state when switching sessions - it will be updated by subsequent events
                    *self.current_session_activity_state.lock().unwrap() = None;

                    // Clear any stop request for the new session to start fresh
                    self.session_stop_requests
                        .lock()
                        .unwrap()
                        .remove(session_id);

                    // Find the current project for this session and update MessagesView
                    let current_project = {
                        let sessions = self.chat_sessions.lock().unwrap();
                        sessions
                            .iter()
                            .find(|s| s.id == *session_id)
                            .map(|s| s.initial_project.clone())
                            .unwrap_or_else(String::new)
                    };

                    warn!("Using initial project: '{}'", current_project);

                    // Update MessagesView with current project and session ID

                    let session_id_for_messages = session_id.clone();
                    self.update_messages_view(cx, |messages_view, _cx| {
                        messages_view.set_current_project(current_project.clone());
                        messages_view.set_current_session_id(Some(session_id_for_messages));
                    });
                }

                // Clear existing messages
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    queue.clear();
                }

                // Get current project for new containers
                let current_project = if let Some(ref session_id) = session_id {
                    let sessions = self.chat_sessions.lock().unwrap();
                    sessions
                        .iter()
                        .find(|s| s.id == *session_id)
                        .map(|s| s.initial_project.clone())
                        .unwrap_or_else(String::new)
                } else {
                    String::new()
                };

                // Process message data with on-demand container creation
                for message_data in messages {
                    let current_container = {
                        let mut queue = self.message_queue.lock().unwrap();

                        // Check if we can reuse the last container (same role)
                        // Note: For user messages, we always create a new container to preserve
                        // node_id and branch_info for each message
                        let needs_new_container = if let Some(last_container) = queue.last() {
                            let last_role = cx
                                .update_entity(last_container, |container, _cx| {
                                    if container.is_user_message() {
                                        MessageRole::User
                                    } else {
                                        MessageRole::Assistant
                                    }
                                })
                                .expect("Failed to get container role");
                            // User messages always get their own container (for branching)
                            last_role == MessageRole::User || last_role != message_data.role
                        } else {
                            true
                        };

                        if needs_new_container {
                            // Create new container for this role
                            let container = cx
                                .new(|cx| {
                                    MessageContainer::with_role(message_data.role.clone(), cx)
                                })
                                .expect("Failed to create message container");

                            // Set current project, node_id, branch_info, and session_id
                            let node_id = message_data.node_id;
                            let branch_info = message_data.branch_info.clone();
                            let sid = session_id.clone();
                            self.update_container(&container, cx, |container, _cx| {
                                container.set_current_project(current_project.clone());
                                container.set_node_id(node_id);
                                container.set_branch_info(branch_info);
                                container.set_session_id(sid);
                            });

                            queue.push(container.clone());
                            container
                        } else {
                            // Use existing container - also set current project in case it changed
                            let container = queue.last().unwrap().clone();
                            let sid = session_id.clone();
                            self.update_container(&container, cx, |container, _cx| {
                                container.set_current_project(current_project.clone());
                                container.set_session_id(sid);
                            });
                            container
                        }
                    }; // Lock is released here

                    // Process fragments into the current container
                    self.process_fragments_for_container(
                        &current_container,
                        message_data.fragments,
                        cx,
                    );
                }

                // Apply tool results to update tool blocks with their execution results
                for tool_result in tool_results {
                    self.update_all_messages(cx, |message_container, cx| {
                        message_container.update_tool_status(
                            &tool_result.tool_id,
                            tool_result.status,
                            tool_result.message.clone(),
                            tool_result.output.clone(),
                            tool_result.duration_seconds,
                            cx,
                        );
                    });
                }

                // Ensure we always end with an Assistant container
                // This is crucial for sessions that are waiting for responses or actively running agents
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    let needs_assistant_container = if let Some(last) = queue.last() {
                        cx.update_entity(last, |message, _cx| message.is_user_message())
                            .expect("Failed to check container role")
                    } else {
                        true // Empty queue needs an assistant container
                    };

                    if needs_assistant_container {
                        let assistant_container = cx
                            .new(|cx| MessageContainer::with_role(MessageRole::Assistant, cx))
                            .expect("Failed to create assistant container");
                        let sid = session_id.clone();
                        self.update_container(&assistant_container, cx, |c, _cx| {
                            c.set_session_id(sid);
                        });
                        queue.push(assistant_container);
                    }
                }

                self.notify_messages_reset(cx);
            }

            UiEvent::StreamingStarted(request_id) => {
                let old_len;
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    old_len = queue.len();

                    // Grab the last container so we can reuse it without holding the lock
                    let last_container = queue.last().cloned();

                    // Check if we need to create a new assistant container
                    let needs_new_container = if let Some(last) = last_container.as_ref() {
                        cx.update_entity(last, |message, _cx| message.is_user_message())
                            .expect("Failed to update entity")
                    } else {
                        true
                    };

                    if needs_new_container {
                        // Create new assistant container
                        let sid = self.current_session_id.lock().unwrap().clone();
                        let assistant_container = cx
                            .new(|cx| {
                                let container =
                                    MessageContainer::with_role(MessageRole::Assistant, cx);
                                container.set_current_request_id(request_id);
                                container.set_session_id(sid);
                                container
                            })
                            .expect("Failed to create new container");
                        queue.push(assistant_container);
                    } else if let Some(last_container) = last_container {
                        // Reuse existing container — no push, just update request_id
                        drop(queue);
                        self.update_container(&last_container, cx, |container, cx| {
                            container.set_current_request_id(request_id);
                            cx.notify();
                        });
                        return;
                    }
                }

                // Sync ListState if we pushed a new container
                self.notify_messages_appended(old_len, cx);
            }
            UiEvent::StreamingStopped {
                id,
                cancelled,
                error: _,
            } => {
                if cancelled {
                    self.update_all_messages(cx, |message_container, cx| {
                        message_container.remove_blocks_with_request_id(id, cx);
                    });
                }
            }
            UiEvent::RollbackStreaming { id } => {
                // Discard all blocks produced by the failed request so the retry
                // starts with a clean slate (same mechanism as cancellation).
                self.update_all_messages(cx, |message_container, cx| {
                    message_container.remove_blocks_with_request_id(id, cx);
                });
            }
            UiEvent::RefreshChatList => {
                debug!("UI: RefreshChatList event received");
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    debug!("UI: Sending ListSessions to backend");
                    let _ = sender.try_send(BackendEvent::ListSessions);
                } else {
                    warn!("UI: No backend event sender available for RefreshChatList");
                }
            }
            UiEvent::UpdateChatList { sessions } => {
                debug!(
                    "UI: UpdateChatList event received with {} sessions",
                    sessions.len()
                );
                // Update local cache
                *self.chat_sessions.lock().unwrap() = sessions.clone();
                let _current_session_id = self.current_session_id.lock().unwrap().clone();

                // Refresh all windows to trigger re-render with new chat data
                debug!("UI: Refreshing windows for chat list update");
                cx.refresh().expect("Failed to refresh windows");
            }

            UiEvent::ClearMessages => {
                debug!("UI: ClearMessages event");
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    queue.clear();
                }
                self.notify_messages_reset(cx);
            }

            UiEvent::SendUserMessage {
                message,
                session_id,
                attachments,
                branch_parent_id,
            } => {
                debug!(
                    "UI: SendUserMessage event for session {}: {} (with {} attachments, branch_parent: {:?})",
                    session_id,
                    message,
                    attachments.len(),
                    branch_parent_id
                );
                // Clear any existing error when user sends a new message
                *self.current_error.lock().unwrap() = None;

                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::SendUserMessage {
                        session_id,
                        message,
                        attachments,
                        branch_parent_id,
                    });
                } else {
                    warn!("UI: No backend event sender available");
                }
            }
            UiEvent::UpdateSessionMetadata { metadata } => {
                debug!(
                    "UI: UpdateSessionMetadata event for session {}",
                    metadata.id
                );
                // Update the specific session in our local cache
                {
                    let mut sessions = self.chat_sessions.lock().unwrap();
                    if let Some(existing_session) =
                        sessions.iter_mut().find(|s| s.id == metadata.id)
                    {
                        *existing_session = metadata.clone();
                        debug!("Updated existing session metadata for {}", metadata.id);
                    } else {
                        // Session not found in cache, add it (shouldn't normally happen)
                        sessions.push(metadata.clone());
                        debug!("Added new session metadata for {}", metadata.id);
                    }
                }

                // If this is the current session, update the current project for parameter filtering

                if let Some(current_session_id) = self.current_session_id.lock().unwrap().as_ref() {
                    if *current_session_id == metadata.id {
                        // Store last_usage for the current session in a stable location
                        // (not in chat_sessions, which can be overwritten by stale disk data)
                        *self.current_session_last_usage.lock().unwrap() =
                            Some(metadata.last_usage.clone());

                        // Update MessagesView with current project
                        self.update_messages_view(cx, |messages_view, _cx| {
                            messages_view.set_current_project(metadata.initial_project.clone());
                        });

                        // Update all MessageContainers with current project
                        self.update_all_messages(cx, |container, _cx| {
                            container.set_current_project(metadata.initial_project.clone());
                        });
                    }
                }

                // Update the chat sidebar entity specifically
                self.update_chat_sidebar(cx, |sidebar, cx| {
                    // Get updated sessions list
                    let updated_sessions = self.chat_sessions.lock().unwrap().clone();
                    sidebar.update_sessions(updated_sessions, cx);
                    cx.notify();
                });
                debug!("UI: Updated chat sidebar for session metadata change");
            }
            UiEvent::UpdateSessionActivityState {
                session_id,
                activity_state,
            } => {
                debug!(
                    "UI: UpdateSessionActivityState event for session {} with state {:?}",
                    session_id, activity_state
                );

                // Update the chat sidebar
                self.update_chat_sidebar(cx, |sidebar, cx| {
                    sidebar.update_single_session_activity_state(
                        session_id.clone(),
                        activity_state.clone(),
                        cx,
                    );
                });

                // Update current session activity state for messages view
                if let Some(current_session_id) = self.current_session_id.lock().unwrap().as_ref() {
                    if current_session_id == &session_id {
                        *self.current_session_activity_state.lock().unwrap() =
                            Some(activity_state.clone());

                        // Show/clear error banner based on session error state.
                        // This ensures the banner appears immediately when the
                        // currently viewed session enters the Errored state, and
                        // clears when it transitions away (e.g. new agent starts).
                        if let crate::session::instance::SessionActivityState::Errored { message } =
                            &activity_state
                        {
                            *self.current_error.lock().unwrap() = Some(message.clone());
                        } else {
                            // Clear any session error when state moves away from Errored
                            // (but only if the current error came from this session —
                            // we check by seeing if there's an error at all; backend
                            // errors are also stored here but those are transient and
                            // would have been cleared by now).
                            *self.current_error.lock().unwrap() = None;
                        }

                        cx.refresh().expect("Failed to refresh windows");
                    }
                }
            }
            UiEvent::QueueUserMessage {
                message,
                session_id,
                attachments,
            } => {
                debug!(
                    "UI: QueueUserMessage event for session {}: {} (with {} attachments)",
                    session_id,
                    message,
                    attachments.len()
                );
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::QueueUserMessage {
                        session_id,
                        message,
                        attachments,
                    });
                }
            }
            UiEvent::RequestPendingMessageEdit { session_id } => {
                debug!(
                    "UI: RequestPendingMessageEdit event for session {}",
                    session_id
                );
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::RequestPendingMessageEdit { session_id });
                }
            }
            UiEvent::UpdatePendingMessage { message } => {
                debug!("UI: UpdatePendingMessage event with message: {:?}", message);
                // Update MessagesView's pending message
                self.update_messages_view(cx, |messages_view, cx| {
                    messages_view.update_pending_message(message.clone());
                    cx.notify();
                });
                // Refresh UI to trigger re-render
                cx.refresh().expect("Failed to refresh windows");
            }
            UiEvent::AddImage { media_type, data } => {
                // Add image to the last message container
                self.update_last_message(cx, |message, cx| {
                    message.add_image_block(media_type, data, cx);
                });
            }

            UiEvent::AppendToolOutput { tool_id, chunk } => {
                // Append tool output to the last message container
                self.update_last_message(cx, |message, cx| {
                    message.append_tool_output(tool_id, chunk, cx);
                });
                // Terminal card height grows as new output lines arrive;
                // keep the chat scrolled to the bottom when following.
                self.auto_scroll_if_following(cx);
            }

            UiEvent::DisplayError { message } => {
                debug!("UI: DisplayError event with message: {}", message);
                // Store the error message in state
                *self.current_error.lock().unwrap() = Some(message);
                // Refresh UI to show the error popover
                cx.refresh().expect("Failed to refresh windows");
            }
            UiEvent::ClearError => {
                debug!("UI: ClearError event");
                // Clear the error message from state
                *self.current_error.lock().unwrap() = None;

                // If the current session is in Errored state, tell the backend
                // to reset it to Idle so the sidebar icon disappears and the
                // error doesn't reappear on next session switch.
                let is_session_errored = self
                    .current_session_activity_state
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|s| {
                        matches!(
                            s,
                            crate::session::instance::SessionActivityState::Errored { .. }
                        )
                    });
                if is_session_errored {
                    if let Some(session_id) = self.current_session_id.lock().unwrap().clone() {
                        if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                            let _ = sender.try_send(BackendEvent::ClearSessionError { session_id });
                        }
                    }
                }

                // Refresh UI to hide the error popover
                cx.refresh().expect("Failed to refresh windows");
            }
            UiEvent::ShowTransientStatus { message } => {
                debug!("UI: ShowTransientStatus: {}", message);
                *self.transient_status.lock().unwrap() = Some(message);
                cx.refresh().expect("Failed to refresh windows");

                // Schedule auto-dismiss after 3 seconds via a background thread
                // that sends ClearTransientStatus back through the event channel.
                let sender = self.event_sender.lock().unwrap().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    let _ = sender.try_send(UiEvent::ClearTransientStatus);
                });
            }
            UiEvent::ClearTransientStatus => {
                *self.transient_status.lock().unwrap() = None;
                cx.refresh().expect("Failed to refresh windows");
            }
            UiEvent::StartReasoningSummaryItem => {
                self.update_last_message(cx, |message, cx| {
                    message.start_reasoning_summary_item(cx);
                });
            }
            UiEvent::AppendReasoningSummaryDelta { delta } => {
                self.update_last_message(cx, |message, cx| {
                    message.append_reasoning_summary_delta(delta, cx);
                });
            }
            UiEvent::CompleteReasoning => {
                self.update_last_message(cx, |message, cx| {
                    message.complete_reasoning(cx);
                });
            }
            UiEvent::UpdateCurrentModel { model_name } => {
                debug!("UI: UpdateCurrentModel event with model: {}", model_name);
                // Store the current model
                *self.current_model.lock().unwrap() = Some(model_name);
                // Refresh UI to update the model selector
                cx.refresh().expect("Failed to refresh windows");
            }
            UiEvent::UpdateSandboxPolicy { policy } => {
                debug!("UI: UpdateSandboxPolicy event with policy: {:?}", policy);
                *self.current_sandbox_policy.lock().unwrap() = Some(policy.clone());
                cx.refresh().expect("Failed to refresh windows");
            }
            UiEvent::UpdateWorktreeData {
                worktrees,
                current_worktree_path,
                is_git_repo,
            } => {
                debug!(
                    "UI: UpdateWorktreeData event — {} worktrees, current_path={:?}, is_git_repo={}",
                    worktrees.len(), current_worktree_path, is_git_repo
                );
                *self.current_worktree_data.lock().unwrap() = Some(WorktreeData {
                    worktrees,
                    current_worktree_path,
                    is_git_repo,
                });
                cx.refresh().expect("Failed to refresh windows");
            }

            // Resource events - logged for now, can be extended for features like "follow mode"
            UiEvent::ResourceLoaded { project, path } => {
                trace!(
                    "UI: ResourceLoaded event - project: {}, path: {}",
                    project,
                    path.display()
                );
            }
            UiEvent::ResourceWritten { project, path } => {
                trace!(
                    "UI: ResourceWritten event - project: {}, path: {}",
                    project,
                    path.display()
                );
            }
            UiEvent::DirectoryListed { project, path } => {
                trace!(
                    "UI: DirectoryListed event - project: {}, path: {}",
                    project,
                    path.display()
                );
            }
            UiEvent::ResourceDeleted { project, path } => {
                trace!(
                    "UI: ResourceDeleted event - project: {}, path: {}",
                    project,
                    path.display()
                );
            }

            UiEvent::CancelSubAgent { tool_id } => {
                debug!("UI: CancelSubAgent event for tool_id: {}", tool_id);
                // Forward to backend with current session ID
                if let Some(session_id) = self.current_session_id.lock().unwrap().clone() {
                    if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                        let _ = sender.try_send(BackendEvent::CancelSubAgent {
                            session_id,
                            tool_id,
                        });
                    }
                } else {
                    warn!("UI: CancelSubAgent requested but no active session");
                }
            }

            // === Session Branching Events ===
            UiEvent::StartMessageEdit {
                session_id,
                node_id,
            } => {
                debug!(
                    "UI: StartMessageEdit event for session {} node {}",
                    session_id, node_id
                );
                // Forward to backend to get message content
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::StartMessageEdit {
                        session_id,
                        node_id,
                    });
                }
            }
            UiEvent::SwitchBranch {
                session_id,
                new_node_id,
            } => {
                debug!(
                    "UI: SwitchBranch event for session {} to node {}",
                    session_id, new_node_id
                );
                // Forward to backend to perform branch switch
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::SwitchBranch {
                        session_id,
                        new_node_id,
                    });
                }
            }

            UiEvent::MessageEditReady {
                content,
                attachments,
                branch_parent_id,
                messages,
                tool_results,
            } => {
                debug!(
                    "UI: MessageEditReady event - content len: {}, attachments: {}, parent: {:?}, {} messages",
                    content.len(),
                    attachments.len(),
                    branch_parent_id,
                    messages.len()
                );

                // Get current session_id without holding lock during SetMessages processing
                let session_id = self.current_session_id.lock().unwrap().clone();

                // Get current project for new containers
                let current_project = if let Some(ref session_id) = session_id {
                    let sessions = self.chat_sessions.lock().unwrap();
                    sessions
                        .iter()
                        .find(|s| s.id == *session_id)
                        .map(|s| s.initial_project.clone())
                        .unwrap_or_default()
                } else {
                    String::new()
                };

                // Clear existing messages and rebuild with truncated set
                // (Inline version of SetMessages logic to avoid recursive call)
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    queue.clear();
                }

                // Update MessagesView with current project and session ID
                if let Some(ref session_id) = session_id {
                    let session_id_for_messages = session_id.clone();
                    self.update_messages_view(cx, |messages_view, _cx| {
                        messages_view.set_current_project(current_project.clone());
                        messages_view.set_current_session_id(Some(session_id_for_messages));
                    });
                }

                // Process message data
                for message_data in messages {
                    let current_container = {
                        let mut queue = self.message_queue.lock().unwrap();

                        let needs_new_container = if let Some(last_container) = queue.last() {
                            let last_role = cx
                                .update_entity(last_container, |container, _cx| {
                                    if container.is_user_message() {
                                        MessageRole::User
                                    } else {
                                        MessageRole::Assistant
                                    }
                                })
                                .expect("Failed to get container role");
                            last_role == MessageRole::User || last_role != message_data.role
                        } else {
                            true
                        };

                        if needs_new_container {
                            let container = cx
                                .new(|cx| {
                                    MessageContainer::with_role(message_data.role.clone(), cx)
                                })
                                .expect("Failed to create message container");

                            let node_id = message_data.node_id;
                            let branch_info = message_data.branch_info.clone();
                            let sid = session_id.clone();
                            self.update_container(&container, cx, |container, _cx| {
                                container.set_current_project(current_project.clone());
                                container.set_node_id(node_id);
                                container.set_branch_info(branch_info);
                                container.set_session_id(sid);
                            });

                            queue.push(container.clone());
                            container
                        } else {
                            let container = queue.last().unwrap().clone();
                            let sid = session_id.clone();
                            self.update_container(&container, cx, |container, _cx| {
                                container.set_current_project(current_project.clone());
                                container.set_session_id(sid);
                            });
                            container
                        }
                    };

                    self.process_fragments_for_container(
                        &current_container,
                        message_data.fragments,
                        cx,
                    );
                }

                // Apply tool results
                for tool_result in tool_results {
                    self.update_all_messages(cx, |message_container, cx| {
                        message_container.update_tool_status(
                            &tool_result.tool_id,
                            tool_result.status,
                            tool_result.message.clone(),
                            tool_result.output.clone(),
                            tool_result.duration_seconds,
                            cx,
                        );
                    });
                }

                // Ensure we end with an Assistant container for the edit response
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    let needs_assistant_container = if let Some(last) = queue.last() {
                        cx.update_entity(last, |message, _cx| message.is_user_message())
                            .expect("Failed to check container role")
                    } else {
                        true
                    };

                    if needs_assistant_container {
                        let assistant_container = cx
                            .new(|cx| MessageContainer::with_role(MessageRole::Assistant, cx))
                            .expect("Failed to create assistant container");
                        let sid = session_id.clone();
                        self.update_container(&assistant_container, cx, |c, _cx| {
                            c.set_session_id(sid);
                        });
                        queue.push(assistant_container);
                    }
                }

                // Sync ListState with fully rebuilt queue
                self.notify_messages_reset(cx);

                // Store pending edit state for RootView to pick up on refresh
                self.set_pending_edit(PendingEdit {
                    content,
                    attachments,
                    branch_parent_id,
                });

                // Refresh UI to trigger RootView to process the pending edit
                cx.refresh().expect("Failed to refresh windows");
            }
            UiEvent::BranchSwitched {
                session_id,
                messages,
                tool_results,
                plan,
            } => {
                debug!(
                    "UI: BranchSwitched event for session {} with {} messages",
                    session_id,
                    messages.len()
                );
                // TODO Phase 4: Update messages display with new branch content
                // For now, we can reuse the SetMessages logic
                self.process_ui_event_async(
                    UiEvent::SetMessages {
                        messages,
                        session_id: Some(session_id),
                        tool_results,
                    },
                    cx,
                );
                self.process_ui_event_async(UiEvent::UpdatePlan { plan }, cx);
            }

            UiEvent::UpdateBranchInfo {
                node_id,
                branch_info,
            } => {
                debug!(
                    "UI: UpdateBranchInfo for node {} with {} siblings",
                    node_id,
                    branch_info.sibling_ids.len()
                );

                // Find the message container with this node_id and update its branch_info
                let queue = self.message_queue.lock().unwrap();
                for container in queue.iter() {
                    let container_node_id = cx
                        .update_entity(container, |c, _cx| c.node_id())
                        .ok()
                        .flatten();

                    if container_node_id == Some(node_id) {
                        let branch_info_clone = branch_info.clone();
                        self.update_container(container, cx, |c, _cx| {
                            c.set_branch_info(Some(branch_info_clone));
                        });
                        break;
                    }
                }

                cx.refresh().expect("Failed to refresh windows");
            }

            UiEvent::PersistUiState => {
                // Cancel any pending save task and start a new one with a debounce
                // delay.  When the timer fires, dirty entries are taken from the
                // store and written to disk on a background thread.
                let task = cx.spawn(async move |cx: &mut gpui::AsyncApp| {
                    gpui::Timer::after(ui_state::debounce_duration()).await;
                    let files = if let Ok(mut store) = ui_state::UiStateStore::global().lock() {
                        store.take_dirty()
                    } else {
                        Vec::new()
                    };
                    if !files.is_empty() {
                        cx.background_spawn(async move {
                            ui_state::write_ui_state_files(files);
                        })
                        .await;
                    }
                });
                *self.ui_state_save_task.lock().unwrap() = Some(task);
            }
        }
    }

    /// Process display fragments and add them to a message container
    fn process_fragments_for_container(
        &self,
        container: &Entity<MessageContainer>,
        fragments: Vec<DisplayFragment>,
        cx: &mut gpui::AsyncApp,
    ) {
        for fragment in fragments {
            match fragment {
                DisplayFragment::PlainText(text) => {
                    self.update_container(container, cx, |container, cx| {
                        container.add_or_append_to_text_block(text, cx);
                    });
                }

                DisplayFragment::ThinkingText {
                    text,
                    duration_seconds,
                } => {
                    self.update_container(container, cx, |container, cx| {
                        container.add_or_append_to_thinking_block_with_duration(
                            text,
                            duration_seconds,
                            cx,
                        );
                    });
                }

                DisplayFragment::ToolName {
                    name,
                    id,
                    duration_seconds,
                } => {
                    self.update_container(container, cx, |container, cx| {
                        container.add_tool_use_block_with_duration(name, id, duration_seconds, cx);
                    });
                }
                DisplayFragment::ToolParameter {
                    name,
                    value,
                    tool_id,
                } => {
                    self.update_container(container, cx, |container, cx| {
                        container.add_or_update_tool_parameter(tool_id, name, value, cx);
                    });
                }
                DisplayFragment::ToolEnd { id } => {
                    self.update_container(container, cx, |container, cx| {
                        container.end_tool_use(id, cx);
                    });
                }
                DisplayFragment::Image { media_type, data } => {
                    self.update_container(container, cx, |container, cx| {
                        container.add_image_block(media_type, data, cx);
                    });
                }
                DisplayFragment::CompactionDivider { summary } => {
                    self.update_container(container, cx, |container, cx| {
                        container.add_compaction_divider(summary.clone(), cx);
                    });
                }
                DisplayFragment::ReasoningSummaryStart => {
                    self.update_container(container, cx, |container, cx| {
                        container.start_reasoning_summary_item(cx);
                    });
                }
                DisplayFragment::ReasoningSummaryDelta(delta) => {
                    self.update_container(container, cx, |container, cx| {
                        container.append_reasoning_summary_delta(delta, cx);
                    });
                }
                DisplayFragment::ToolOutput { tool_id, chunk } => {
                    self.update_container(container, cx, |container, cx| {
                        container.append_tool_output(tool_id, chunk, cx);
                    });
                }

                DisplayFragment::ToolTerminal { .. } => {
                    // The GPUI terminal executor registers the tool→terminal
                    // mapping directly in the TerminalPool, so no action needed
                    // during fragment replay.
                }

                DisplayFragment::ReasoningComplete => {
                    self.update_container(container, cx, |container, cx| {
                        container.complete_reasoning(cx);
                    });
                }
                DisplayFragment::HiddenToolCompleted => {
                    self.update_container(container, cx, |container, cx| {
                        container.mark_hidden_tool_completed(cx);
                    });
                }
            }
        }
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

    // Handle backend responses
    fn handle_backend_response(&self, response: BackendResponse, cx: &mut AsyncApp) {
        match response {
            BackendResponse::SessionCreated { session_id } => {
                debug!("Received BackendResponse::SessionCreated");
                *self.current_session_id.lock().unwrap() = Some(session_id.clone());
                // Refresh the session list
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::ListSessions);
                    // Load the newly created session to connect it to the UI
                    let _ = sender.try_send(BackendEvent::LoadSession { session_id });
                }
            }

            BackendResponse::SessionDeleted { session_id } => {
                debug!("Received BackendResponse::SessionDeleted");
                // Clean up collapse-state overrides for the deleted session
                elements::ToolCollapseState::remove_session(&session_id);

                // If the deleted session was the currently active one, disconnect
                // from it so the messages view shows the "no session" hint.
                // (This may already have been done eagerly in the delete-request
                // handler in root.rs, in which case the check is a no-op.)
                let is_current =
                    self.current_session_id.lock().unwrap().as_deref() == Some(session_id.as_str());
                if is_current {
                    debug!("Deleted session was the active session — clearing UI state");
                    self.clear_current_session_state();

                    // Tell MessagesView there is no session
                    self.update_messages_view(cx, |view, cx| {
                        view.set_current_session_id(None);
                        view.messages_reset(0);
                        cx.notify();
                    });
                }

                // Refresh the session list
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::ListSessions);
                }
                cx.refresh().expect("Failed to refresh windows");
            }
            BackendResponse::SessionsListed { sessions } => {
                debug!("Received BackendResponse::SessionsListed");
                *self.chat_sessions.lock().unwrap() = sessions.clone();
                self.push_event(UiEvent::UpdateChatList { sessions });
            }
            BackendResponse::Error { message } => {
                warn!("Backend error: {}", message);
                // Display the error to the user
                self.push_event(UiEvent::DisplayError { message });
            }
            BackendResponse::PendingMessageForEdit {
                session_id,
                message: _,
            } => {
                debug!(
                    "Received BackendResponse::PendingMessageForEdit for session {}",
                    session_id
                );
                // TODO: Move pending message to text input field for editing
                // For now, clear the pending message display
                self.push_event(UiEvent::UpdatePendingMessage { message: None });
            }
            BackendResponse::PendingMessageUpdated {
                session_id,
                message,
            } => {
                debug!(
                    "Received BackendResponse::PendingMessageUpdated for session {}",
                    session_id
                );
                // Only update pending message display if this is for the current session
                if let Some(current_session_id) = self.current_session_id.lock().unwrap().as_ref() {
                    if current_session_id == &session_id {
                        self.push_event(UiEvent::UpdatePendingMessage { message });
                    }
                }
            }
            BackendResponse::ModelSwitched {
                session_id,
                model_name,
            } => {
                let current_session_id = self.current_session_id.lock().unwrap().clone();
                if current_session_id.as_deref() == Some(session_id.as_str()) {
                    debug!(
                        "Received BackendResponse::ModelSwitched for active session {}: {}",
                        session_id, model_name
                    );
                    self.push_event(UiEvent::UpdateCurrentModel {
                        model_name: model_name.clone(),
                    });
                } else {
                    debug!(
                        "Ignoring BackendResponse::ModelSwitched for session {} (current: {:?})",
                        session_id, current_session_id
                    );
                }
            }

            BackendResponse::SandboxPolicyChanged { session_id, policy } => {
                let current_session_id = self.current_session_id.lock().unwrap().clone();
                if current_session_id.as_deref() == Some(session_id.as_str()) {
                    debug!(
                        "Received BackendResponse::SandboxPolicyChanged for active session {}",
                        session_id
                    );
                    self.push_event(UiEvent::UpdateSandboxPolicy { policy });
                } else {
                    debug!(
                        "Ignoring BackendResponse::SandboxPolicyChanged for session {} (current: {:?})",
                        session_id, current_session_id
                    );
                }
            }

            BackendResponse::SubAgentCancelled {
                session_id,
                tool_id,
            } => {
                debug!(
                    "Received BackendResponse::SubAgentCancelled for tool {} in session {}",
                    tool_id, session_id
                );
                // The sub-agent will update its own UI state via the normal tool output mechanism
                // No additional UI update needed here
            }

            // Session branching responses
            BackendResponse::MessageEditReady {
                session_id,
                content,
                attachments,
                branch_parent_id,
                messages,
                tool_results,
            } => {
                debug!(
                    "Received BackendResponse::MessageEditReady for session {} with {} chars, {} attachments, {} messages",
                    session_id,
                    content.len(),
                    attachments.len(),
                    messages.len()
                );

                // Forward to UI as event
                self.process_ui_event_async(
                    UiEvent::MessageEditReady {
                        content: content.clone(),
                        attachments: attachments.clone(),
                        branch_parent_id,
                        messages: messages.clone(),
                        tool_results: tool_results.clone(),
                    },
                    cx,
                );
            }

            BackendResponse::BranchSwitched {
                session_id,
                messages,
                tool_results,
                plan,
            } => {
                debug!(
                    "Received BackendResponse::BranchSwitched for session {} with {} messages",
                    session_id,
                    messages.len()
                );
                // Forward to UI as event
                self.process_ui_event_async(
                    UiEvent::BranchSwitched {
                        session_id: session_id.clone(),
                        messages: messages.clone(),
                        tool_results: tool_results.clone(),
                        plan: plan.clone(),
                    },
                    cx,
                );
            }
            BackendResponse::MessageEditCancelled {
                session_id,
                messages,
                tool_results,
            } => {
                debug!(
                    "Received BackendResponse::MessageEditCancelled for session {} with {} messages",
                    session_id,
                    messages.len()
                );

                // Forward to UI as event - reuse SetMessages to restore the view
                self.process_ui_event_async(
                    UiEvent::SetMessages {
                        messages: messages.clone(),
                        session_id: Some(session_id.clone()),
                        tool_results: tool_results.clone(),
                    },
                    cx,
                );
            }

            // Git worktree responses — forwarded to the WorktreeSelector component
            BackendResponse::BranchesAndWorktreesListed {
                session_id,
                worktrees,
                current_branch: _,
                is_git_repo,
                ..
            } => {
                let current_session_id = self.current_session_id.lock().unwrap().clone();
                if current_session_id.as_deref() == Some(session_id.as_str()) {
                    debug!(
                        "Received BranchesAndWorktreesListed for active session {}: {} worktrees, is_git_repo={}",
                        session_id, worktrees.len(), is_git_repo
                    );
                    // Preserve the current selection (set by WorktreeSwitched / WorktreeCreated)
                    let existing_path = self
                        .current_worktree_data
                        .lock()
                        .unwrap()
                        .as_ref()
                        .and_then(|d| d.current_worktree_path.clone());
                    self.push_event(UiEvent::UpdateWorktreeData {
                        worktrees,
                        current_worktree_path: existing_path,
                        is_git_repo,
                    });
                } else {
                    debug!(
                        "Ignoring BranchesAndWorktreesListed for session {} (current: {:?})",
                        session_id, current_session_id
                    );
                }
            }
            BackendResponse::WorktreeSwitched {
                session_id,
                worktree_path,
                branch,
            } => {
                let current_session_id = self.current_session_id.lock().unwrap().clone();
                if current_session_id.as_deref() == Some(session_id.as_str()) {
                    info!(
                        "Worktree switched for active session {}: path={:?}, branch={:?}",
                        session_id, worktree_path, branch
                    );
                    // Update the stored worktree data with the new selection, preserving the list
                    let current_data = self.current_worktree_data.lock().unwrap().clone();
                    let (worktrees, is_git_repo) = current_data
                        .map(|d| (d.worktrees, d.is_git_repo))
                        .unwrap_or_default();
                    self.push_event(UiEvent::UpdateWorktreeData {
                        worktrees,
                        current_worktree_path: worktree_path,
                        is_git_repo,
                    });
                    // Also refresh the full list since worktrees may have changed
                    if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                        let _ =
                            sender.try_send(BackendEvent::ListBranchesAndWorktrees { session_id });
                    }
                }
            }
            BackendResponse::WorktreeCreated {
                session_id,
                worktree_path,
                branch,
            } => {
                let current_session_id = self.current_session_id.lock().unwrap().clone();
                if current_session_id.as_deref() == Some(session_id.as_str()) {
                    info!(
                        "Worktree created for active session {}: {:?} (branch: {})",
                        session_id, worktree_path, branch
                    );
                    // Update selection to the newly created worktree, preserving the existing list
                    let current_data = self.current_worktree_data.lock().unwrap().clone();
                    let (worktrees, is_git_repo) = current_data
                        .map(|d| (d.worktrees, d.is_git_repo))
                        .unwrap_or_default();
                    self.push_event(UiEvent::UpdateWorktreeData {
                        worktrees,
                        current_worktree_path: Some(worktree_path),
                        is_git_repo,
                    });
                    // Refresh the full list to include the new worktree
                    if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                        let _ =
                            sender.try_send(BackendEvent::ListBranchesAndWorktrees { session_id });
                    }
                }
            }
            BackendResponse::ProjectAdded {
                project_name,
                session_id,
            } => {
                info!(
                    "Project '{}' added, initial session: {}",
                    project_name, session_id
                );
                *self.current_session_id.lock().unwrap() = Some(session_id.clone());
                // Refresh the session list and load the new session
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::ListSessions);
                    let _ = sender.try_send(BackendEvent::LoadSession { session_id });
                }
            }
        }
    }
}

#[async_trait]
impl UserInterface for Gpui {
    async fn send_event(&self, event: UiEvent) -> Result<(), UIError> {
        // Handle special events that need state management
        match &event {
            UiEvent::StreamingStarted(request_id) => {
                // Store the request ID

                *self.current_request_id.lock().unwrap() = *request_id;
                // Clear any existing error/notification when new operation starts
                *self.current_error.lock().unwrap() = None;
                *self.transient_status.lock().unwrap() = None;
            }
            UiEvent::StreamingStopped { .. } => {
                // Clear stop request for current session since streaming has stopped
                if let Some(current_session_id) = self.current_session_id.lock().unwrap().as_ref() {
                    self.session_stop_requests
                        .lock()
                        .unwrap()
                        .remove(current_session_id);
                }
            }
            UiEvent::UpdateSandboxPolicy { policy } => {
                *self.current_sandbox_policy.lock().unwrap() = Some(policy.clone());
            }
            _ => {}
        }

        // Forward all events to the event processing
        self.push_event(event);
        Ok(())
    }

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        match fragment {
            DisplayFragment::PlainText(text) => {
                self.push_event(UiEvent::AppendToTextBlock {
                    content: text.clone(),
                });
            }

            DisplayFragment::ThinkingText { ref text, .. } => {
                self.push_event(UiEvent::AppendToThinkingBlock {
                    content: text.clone(),
                });
            }
            DisplayFragment::ToolName { name, id, .. } => {
                if id.is_empty() {
                    warn!(
                        "StreamingProcessor provided empty tool ID for tool '{}' - this is a bug!",
                        name
                    );
                    return Err(UIError::IOError(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Empty tool ID for tool '{name}'"),
                    )));
                }

                self.push_event(UiEvent::StartTool {
                    name: name.clone(),
                    id: id.clone(),
                });
            }
            DisplayFragment::ToolParameter {
                name,
                value,
                tool_id,
            } => {
                if tool_id.is_empty() {
                    error!("StreamingProcessor provided empty tool ID for parameter '{}' - this is a bug!", name);
                    return Err(UIError::IOError(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Empty tool ID for parameter '{name}'"),
                    )));
                }

                self.push_event(UiEvent::UpdateToolParameter {
                    tool_id: tool_id.clone(),
                    name: name.clone(),
                    value: value.clone(),
                    replace: false,
                });
            }
            DisplayFragment::ToolEnd { id } => {
                if id.is_empty() {
                    warn!("StreamingProcessor provided empty tool ID for ToolEnd - this is a bug!");
                    return Err(UIError::IOError(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Empty tool ID for ToolEnd".to_string(),
                    )));
                }

                self.push_event(UiEvent::EndTool { id: id.clone() });
            }
            DisplayFragment::Image { media_type, data } => {
                self.push_event(UiEvent::AddImage {
                    media_type: media_type.clone(),
                    data: data.clone(),
                });
            }
            DisplayFragment::ReasoningSummaryStart => {
                self.push_event(UiEvent::StartReasoningSummaryItem);
            }
            DisplayFragment::ReasoningSummaryDelta(delta) => {
                self.push_event(UiEvent::AppendReasoningSummaryDelta {
                    delta: delta.clone(),
                });
            }
            DisplayFragment::ReasoningComplete => {
                self.push_event(UiEvent::CompleteReasoning);
            }
            DisplayFragment::ToolOutput { tool_id, chunk } => {
                if tool_id.is_empty() {
                    warn!(
                        "StreamingProcessor provided empty tool ID for ToolOutput - this is a bug!"
                    );
                    return Err(UIError::IOError(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Empty tool ID for ToolOutput".to_string(),
                    )));
                }

                self.push_event(UiEvent::AppendToolOutput {
                    tool_id: tool_id.clone(),
                    chunk: chunk.clone(),
                });
            }

            DisplayFragment::ToolTerminal { .. } => {
                // The GPUI terminal executor registers the tool→terminal
                // mapping directly in the TerminalPool, so no event needed.
            }

            DisplayFragment::CompactionDivider { summary } => {
                self.push_event(UiEvent::DisplayCompactionSummary {
                    summary: summary.clone(),
                });
            }
            DisplayFragment::HiddenToolCompleted => {
                self.push_event(UiEvent::HiddenToolCompleted);
            }
        }

        Ok(())
    }

    fn should_streaming_continue(&self) -> bool {
        // Check if the current session has requested a stop
        if let Some(current_session_id) = self.current_session_id.lock().unwrap().as_ref() {
            let stop_requests = self.session_stop_requests.lock().unwrap();
            if stop_requests.contains(current_session_id) {
                return false;
            }
        }

        // Default: continue streaming
        true
    }

    fn notify_rate_limit(&self, _seconds_remaining: u64) {
        // This is not handled here, but in the ProxyUI of each SessionInstance.
        // We receive separate events for SessionActivityState
    }

    fn clear_rate_limit(&self) {
        // See notify_rate_limit()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
