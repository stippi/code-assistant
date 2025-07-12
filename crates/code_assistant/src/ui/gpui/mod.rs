pub mod assets;
pub mod auto_scroll;
pub mod chat_sidebar;
pub mod content_renderer;
pub mod diff_renderer;
pub mod elements;
pub mod file_icons;
mod memory;
mod messages;
pub mod parameter_renderers;
mod path_util;
mod root;
pub mod simple_renderers;
pub mod theme;

use crate::persistence::ChatMetadata;
use crate::types::WorkingMemory;
use crate::ui::gpui::{
    content_renderer::ContentRenderer,
    diff_renderer::DiffParameterRenderer,
    elements::MessageRole,
    parameter_renderers::{DefaultParameterRenderer, ParameterRendererRegistry},
    simple_renderers::SimpleParameterRenderer,
};
use crate::ui::{async_trait, DisplayFragment, StreamingState, UIError, UiEvent, UserInterface};
use assets::Assets;
use async_channel;
use gpui::{
    actions, px, App, AppContext, AsyncApp, Entity, Global, KeyBinding, Menu, MenuItem, Point,
    SharedString,
};
use gpui_component::input::InputState;
use gpui_component::Root;
pub use memory::MemoryView;
pub use messages::MessagesView;
pub use root::RootView;

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, error, trace, warn};

use elements::MessageContainer;

actions!(code_assistant, [Quit, CloseWindow]);

// Global UI event sender for chat components
#[derive(Clone)]
pub struct UiEventSender(pub async_channel::Sender<UiEvent>);

impl Global for UiEventSender {}

// Unified event type for all UI→Backend communication
#[derive(Debug, Clone)]
pub enum BackendEvent {
    // Session management
    LoadSession { session_id: String },
    CreateNewSession { name: Option<String> },
    DeleteSession { session_id: String },
    ListSessions,

    // Agent operations
    SendUserMessage { session_id: String, message: String },
}

// Response from backend to UI
#[derive(Debug, Clone)]
pub enum BackendResponse {
    SessionCreated {
        session_id: String,
    },
    #[allow(dead_code)]
    SessionDeleted {
        session_id: String,
    },
    SessionsListed {
        sessions: Vec<ChatMetadata>,
    },
    Error {
        message: String,
    },
}

// Our main UI struct that implements the UserInterface trait
#[derive(Clone)]
pub struct Gpui {
    message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
    input_value: Arc<Mutex<Option<String>>>,
    input_requested: Arc<Mutex<bool>>,
    working_memory: Arc<Mutex<Option<WorkingMemory>>>,
    event_sender: Arc<Mutex<async_channel::Sender<UiEvent>>>,
    event_receiver: Arc<Mutex<async_channel::Receiver<UiEvent>>>,
    event_task: Arc<Mutex<Option<gpui::Task<()>>>>,
    session_event_task: Arc<Mutex<Option<gpui::Task<()>>>>,
    current_request_id: Arc<Mutex<u64>>,
    #[allow(dead_code)]
    parameter_renderers: Arc<ParameterRendererRegistry>,
    streaming_state: Arc<Mutex<StreamingState>>,
    // Unified backend communication
    backend_event_sender: Arc<Mutex<Option<async_channel::Sender<BackendEvent>>>>,
    backend_response_receiver: Arc<Mutex<Option<async_channel::Receiver<BackendResponse>>>>,

    // Current chat state
    current_session_id: Arc<Mutex<Option<String>>>,
    chat_sessions: Arc<Mutex<Vec<ChatMetadata>>>,

    // UI components
    chat_sidebar: Arc<Mutex<Option<Entity<chat_sidebar::ChatSidebar>>>>,
}

fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);

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
    pub fn new() -> Self {
        let message_queue = Arc::new(Mutex::new(Vec::new()));
        let input_value = Arc::new(Mutex::new(None));
        let input_requested = Arc::new(Mutex::new(false));
        let working_memory = Arc::new(Mutex::new(None));
        let event_task = Arc::new(Mutex::new(None::<gpui::Task<()>>));
        let session_event_task = Arc::new(Mutex::new(None::<gpui::Task<()>>));
        let current_request_id = Arc::new(Mutex::new(0));
        let streaming_state = Arc::new(Mutex::new(StreamingState::Idle));

        // Initialize parameter renderers registry with default renderer
        let mut registry = ParameterRendererRegistry::new(Box::new(DefaultParameterRenderer));

        // Register specialized renderers
        registry.register_renderer(Box::new(DiffParameterRenderer));
        registry.register_renderer(Box::new(ContentRenderer));

        // Register simple renderers for parameters that don't need labels
        registry.register_renderer(Box::new(SimpleParameterRenderer::new(
            vec![
                ("execute_command".to_string(), "command_line".to_string()),
                ("read_files".to_string(), "paths".to_string()),
                ("list_files".to_string(), "paths".to_string()),
                ("replace_in_file".to_string(), "path".to_string()),
                ("write_file".to_string(), "path".to_string()),
                ("search_files".to_string(), "regex".to_string()),
            ],
            false, // These are not full-width
        )));

        // Wrap the registry in Arc for sharing
        let parameter_renderers = Arc::new(registry);

        // Set the global registry
        ParameterRendererRegistry::set_global(parameter_renderers.clone());

        // Create a channel to send and receive UiEvents
        let (tx, rx) = async_channel::unbounded::<UiEvent>();
        let event_sender = Arc::new(Mutex::new(tx));
        let event_receiver = Arc::new(Mutex::new(rx));

        Self {
            message_queue,
            input_value,
            input_requested,
            working_memory,
            event_sender,
            event_receiver,
            event_task,
            session_event_task,
            current_request_id,
            parameter_renderers,
            streaming_state,
            backend_event_sender: Arc::new(Mutex::new(None)),
            backend_response_receiver: Arc::new(Mutex::new(None)),

            current_session_id: Arc::new(Mutex::new(None)),
            chat_sessions: Arc::new(Mutex::new(Vec::new())),

            chat_sidebar: Arc::new(Mutex::new(None)),
        }
    }

    // Run the application
    pub fn run_app(&self) {
        let message_queue = self.message_queue.clone();
        let input_value = self.input_value.clone();
        let input_requested = self.input_requested.clone();
        let working_memory = self.working_memory.clone();
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

            // Initialize file icons
            file_icons::init(cx);

            // Initialize gpui-component modules
            gpui_component::init(cx);
            // Apply our custom theme colors
            theme::init_themes(cx);

            init(cx);

            // Spawn task to receive UiEvents
            let rx = gpui_clone.event_receiver.lock().unwrap().clone();
            let async_gpui_clone = gpui_clone.clone();
            debug!("Starting UI event processing task");
            let task = cx.spawn(async move |cx: &mut AsyncApp| {
                debug!("UI event processing task is running");
                loop {
                    trace!("Waiting for UI event...");
                    let result = rx.recv().await;
                    match result {
                        Ok(received_event) => {
                            trace!("UI event processing: Received event: {:?}", received_event);
                            async_gpui_clone.process_ui_event_async(received_event, cx);
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
                // Wait a bit for the communication channels to be set up
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

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
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    }
                }
            });

            // Store the chat response task as well
            {
                let mut task_guard = gpui_clone.session_event_task.lock().unwrap();
                *task_guard = Some(chat_response_task);
            }

            // Create memory view with our shared working memory
            let memory_view = cx.new(|cx| MemoryView::new(working_memory.clone(), cx));

            // Create window with larger size to accommodate chat sidebar, messages, and memory view
            let bounds =
                gpui::Bounds::centered(None, gpui::size(gpui::px(1400.0), gpui::px(700.0)), cx);
            // Open window with titlebar
            let window = cx
                .open_window(
                    gpui::WindowOptions {
                        window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                        titlebar: Some(gpui::TitlebarOptions {
                            title: Some(gpui::SharedString::from("Code Assistant")),
                            appears_transparent: true,
                            traffic_light_position: Some(Point {
                                x: px(16.),
                                y: px(16.),
                            }),
                        }),
                        ..Default::default()
                    },
                    |window, cx| {
                        // Create TextInput with multi-line support
                        let text_input = cx.new(|cx| {
                            InputState::new(window, cx)
                                .multi_line()
                                .auto_grow(1, 8)
                                .placeholder("Type your message...")
                        });

                        // Create MessagesView
                        let messages_view =
                            cx.new(|cx| MessagesView::new(message_queue.clone(), cx));

                        // Create ChatSidebar and store it in Gpui
                        let chat_sidebar = cx.new(|cx| chat_sidebar::ChatSidebar::new(cx));
                        *gpui_clone.chat_sidebar.lock().unwrap() = Some(chat_sidebar.clone());

                        // Create RootView
                        let root_view = cx.new(|cx| {
                            RootView::new(
                                text_input,
                                memory_view.clone(),
                                messages_view,
                                chat_sidebar.clone(),
                                cx,
                                input_value.clone(),
                                input_requested.clone(),
                                gpui_clone.streaming_state.clone(),
                            )
                        });

                        // Wrap in Root component
                        cx.new(|cx| Root::new(root_view.into(), window, cx))
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
            UiEvent::DisplayUserInput { content } => {
                let mut queue = self.message_queue.lock().unwrap();
                let result = cx.new(|cx| {
                    let new_message = MessageContainer::with_role(MessageRole::User, cx);
                    new_message.add_text_block(&content, cx);
                    new_message
                });
                if let Ok(new_message) = result {
                    queue.push(new_message);
                } else {
                    warn!("Failed to create message entity");
                }
            }
            UiEvent::AppendToTextBlock { content } => {
                let queue = self.message_queue.lock().unwrap();
                if let Some(last) = queue.last() {
                    // Since StreamingStarted ensures last container is Assistant, we can safely append
                    cx.update_entity(&last, |message, cx| {
                        message.add_or_append_to_text_block(&content, cx)
                    })
                    .expect("Failed to update entity");
                }
            }
            UiEvent::AppendToThinkingBlock { content } => {
                let queue = self.message_queue.lock().unwrap();
                if let Some(last) = queue.last() {
                    // Since StreamingStarted ensures last container is Assistant, we can safely append
                    cx.update_entity(&last, |message, cx| {
                        message.add_or_append_to_thinking_block(&content, cx)
                    })
                    .expect("Failed to update entity");
                }
            }
            UiEvent::StartTool { name, id } => {
                let queue = self.message_queue.lock().unwrap();
                if let Some(last) = queue.last() {
                    // Since StreamingStarted ensures last container is Assistant, we can safely add tool
                    cx.update_entity(&last, |message, cx| {
                        message.add_tool_use_block(&name, &id, cx);
                    })
                    .expect("Failed to update entity");
                }
            }
            UiEvent::UpdateToolParameter {
                tool_id,
                name,
                value,
            } => {
                let queue = self.message_queue.lock().unwrap();
                if let Some(last) = queue.last() {
                    cx.update_entity(&last, |message, cx| {
                        message.add_or_update_tool_parameter(&tool_id, &name, &value, cx);
                    })
                    .expect("Failed to update entity");
                }
            }
            UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
                output,
            } => {
                let queue = self.message_queue.lock().unwrap();
                for message_container in queue.iter() {
                    cx.update_entity(&message_container, |message_container, cx| {
                        message_container.update_tool_status(
                            &tool_id,
                            status,
                            message.clone(),
                            output.clone(),
                            cx,
                        );
                    })
                    .expect("Failed to update entity");
                }
            }
            UiEvent::EndTool { id } => {
                let queue = self.message_queue.lock().unwrap();
                for message_container in queue.iter() {
                    cx.update_entity(&message_container, |message_container, cx| {
                        message_container.end_tool_use(&id, cx);
                    })
                    .expect("Failed to update entity");
                }
            }
            UiEvent::UpdateMemory { memory } => {
                if let Ok(mut memory_guard) = self.working_memory.lock() {
                    *memory_guard = Some(memory);
                }
                cx.refresh().expect("Failed to refresh windows");
            }
            UiEvent::SetMessages {
                messages,
                session_id,
                tool_results,
            } => {
                // Update current session ID if provided
                if let Some(session_id) = session_id {
                    *self.current_session_id.lock().unwrap() = Some(session_id);
                }

                // Clear existing messages
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    queue.clear();
                }

                // Process message data with on-demand container creation
                for message_data in messages {
                    let current_container = {
                        let mut queue = self.message_queue.lock().unwrap();

                        // Check if we can reuse the last container (same role)
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
                            // Create new container for this role
                            let container = cx
                                .new(|cx| MessageContainer::with_role(message_data.role, cx))
                                .expect("Failed to create message container");
                            queue.push(container.clone());
                            container
                        } else {
                            // Use existing container
                            queue.last().unwrap().clone()
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
                    let queue = self.message_queue.lock().unwrap();
                    for message_container in queue.iter() {
                        cx.update_entity(message_container, |message_container, cx| {
                            message_container.update_tool_status(
                                &tool_result.tool_id,
                                tool_result.status,
                                tool_result.message.clone(),
                                tool_result.output.clone(),
                                cx,
                            );
                        })
                        .expect("Failed to update entity");
                    }
                }

                cx.refresh().expect("Failed to refresh windows");
            }
            UiEvent::StreamingStarted(request_id) => {
                let mut queue = self.message_queue.lock().unwrap();

                // Check if we need to create a new assistant container
                let needs_new_container = if let Some(last) = queue.last() {
                    cx.update_entity(&last, |message, _cx| message.is_user_message())
                        .expect("Failed to update entity")
                } else {
                    true
                };

                if needs_new_container {
                    // Create new assistant container
                    let assistant_container = cx
                        .new(|cx| {
                            let container = MessageContainer::with_role(MessageRole::Assistant, cx);
                            container.set_current_request_id(request_id);
                            container.set_waiting_for_content(true);
                            container
                        })
                        .expect("Failed to create new container");
                    queue.push(assistant_container);
                } else {
                    // Use existing assistant container
                    if let Some(last_message) = queue.last() {
                        cx.update_entity(last_message, |container, cx| {
                            container.set_current_request_id(request_id);
                            container.set_waiting_for_content(true);
                            cx.notify();
                        })
                        .expect("Failed to update existing container");
                    }
                }
            }
            UiEvent::StreamingStopped { id, cancelled } => {
                if cancelled {
                    let queue = self.message_queue.lock().unwrap();
                    for message_container in queue.iter() {
                        cx.update_entity(message_container, |message_container, cx| {
                            message_container.remove_blocks_with_request_id(id, cx);
                        })
                        .expect("Failed to update entity");
                    }
                }
            }
            // Chat management events - forward to backend thread
            UiEvent::LoadChatSession { session_id } => {
                debug!("UI: LoadChatSession event for session_id: {}", session_id);
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::LoadSession { session_id });
                }
            }
            UiEvent::CreateNewChatSession { name } => {
                debug!("UI: CreateNewChatSession event with name: {:?}", name);
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::CreateNewSession { name });
                }
            }
            UiEvent::DeleteChatSession { session_id } => {
                debug!("UI: DeleteChatSession event for session_id: {}", session_id);
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::DeleteSession { session_id });
                }
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
                let mut queue = self.message_queue.lock().unwrap();
                queue.clear();
                cx.refresh().expect("Failed to refresh windows");
            }
            UiEvent::SendUserMessage {
                message,
                session_id,
            } => {
                debug!(
                    "UI: SendUserMessage event for session {}: {}",
                    session_id, message
                );
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::SendUserMessage {
                        session_id,
                        message,
                    });
                } else {
                    warn!("UI: No backend event sender available");
                }
            }
            UiEvent::RateLimitNotification { seconds_remaining } => {
                debug!(
                    "UI: RateLimitNotification event: {} seconds remaining",
                    seconds_remaining
                );
                let queue = self.message_queue.lock().unwrap();
                if let Some(last) = queue.last() {
                    cx.update_entity(&last, |message, cx| {
                        message.set_rate_limit_countdown(Some(seconds_remaining));
                        cx.notify();
                    })
                    .expect("Failed to update entity");
                }
            }
            UiEvent::ClearRateLimit => {
                debug!("UI: ClearRateLimit event");
                let queue = self.message_queue.lock().unwrap();
                if let Some(last) = queue.last() {
                    cx.update_entity(&last, |message, cx| {
                        message.set_rate_limit_countdown(None);
                        cx.notify();
                    })
                    .expect("Failed to update entity");
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

                // Update the chat sidebar entity specifically
                if let Some(chat_sidebar_entity) = self.chat_sidebar.lock().unwrap().as_ref() {
                    cx.update_entity(chat_sidebar_entity, |sidebar, cx| {
                        // Get updated sessions list
                        let updated_sessions = self.chat_sessions.lock().unwrap().clone();
                        sidebar.update_sessions(updated_sessions, cx);
                        cx.notify();
                    })
                    .expect("Failed to update chat sidebar entity");
                    debug!("UI: Updated chat sidebar for session metadata change");
                } else {
                    debug!("UI: No chat sidebar entity available for metadata update");
                }
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
                    cx.update_entity(container, |container, cx| {
                        container.add_or_append_to_text_block(text, cx);
                    })
                    .expect("Failed to update entity");
                }
                DisplayFragment::ThinkingText(text) => {
                    cx.update_entity(container, |container, cx| {
                        container.add_or_append_to_thinking_block(text, cx);
                    })
                    .expect("Failed to update entity");
                }
                DisplayFragment::ToolName { name, id } => {
                    cx.update_entity(container, |container, cx| {
                        container.add_tool_use_block(name, id, cx);
                    })
                    .expect("Failed to update entity");
                }
                DisplayFragment::ToolParameter {
                    name,
                    value,
                    tool_id,
                } => {
                    cx.update_entity(container, |container, cx| {
                        container.add_or_update_tool_parameter(tool_id, name, value, cx);
                    })
                    .expect("Failed to update entity");
                }
                DisplayFragment::ToolEnd { id } => {
                    cx.update_entity(container, |container, cx| {
                        container.end_tool_use(id, cx);
                    })
                    .expect("Failed to update entity");
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

    // Handle backend responses
    fn handle_backend_response(&self, response: BackendResponse, _cx: &mut AsyncApp) {
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
            BackendResponse::SessionDeleted { session_id: _ } => {
                debug!("Received BackendResponse::SessionDeleted");
                // Refresh the session list
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::ListSessions);
                }
            }
            BackendResponse::SessionsListed { sessions } => {
                debug!("Received BackendResponse::SessionsListed");
                *self.chat_sessions.lock().unwrap() = sessions.clone();
                self.push_event(UiEvent::UpdateChatList { sessions });
            }
            BackendResponse::Error { message } => {
                warn!("Backend error: {}", message);
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
                // Set streaming state to Streaming
                *self.streaming_state.lock().unwrap() = StreamingState::Streaming;
                // Store the request ID
                *self.current_request_id.lock().unwrap() = *request_id;
            }
            UiEvent::StreamingStopped { .. } => {
                // Reset streaming state to Idle
                *self.streaming_state.lock().unwrap() = StreamingState::Idle;
            }
            _ => {}
        }

        // Forward all events to the event processing
        self.push_event(event);
        Ok(())
    }

    async fn get_input(&self) -> Result<String, UIError> {
        // Request input
        {
            let mut requested = self.input_requested.lock().unwrap();
            *requested = true;
        }

        // Wait for input or commands
        loop {
            // Check for user input
            {
                let mut input = self.input_value.lock().unwrap();
                if let Some(value) = input.take() {
                    // Reset input request
                    let mut requested = self.input_requested.lock().unwrap();
                    *requested = false;
                    return Ok(value);
                }
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), UIError> {
        match fragment {
            DisplayFragment::PlainText(text) => {
                self.push_event(UiEvent::AppendToTextBlock {
                    content: text.clone(),
                });
            }
            DisplayFragment::ThinkingText(text) => {
                self.push_event(UiEvent::AppendToThinkingBlock {
                    content: text.clone(),
                });
            }
            DisplayFragment::ToolName { name, id } => {
                if id.is_empty() {
                    warn!(
                        "StreamingProcessor provided empty tool ID for tool '{}' - this is a bug!",
                        name
                    );
                    return Err(UIError::IOError(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Empty tool ID for tool '{}'", name),
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
                        format!("Empty tool ID for parameter '{}'", name),
                    )));
                }

                self.push_event(UiEvent::UpdateToolParameter {
                    tool_id: tool_id.clone(),
                    name: name.clone(),
                    value: value.clone(),
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
        }

        Ok(())
    }

    fn should_streaming_continue(&self) -> bool {
        match *self.streaming_state.lock().unwrap() {
            StreamingState::StopRequested => false,
            _ => true,
        }
    }

    fn notify_rate_limit(&self, seconds_remaining: u64) {
        self.push_event(UiEvent::RateLimitNotification { seconds_remaining });
    }

    fn clear_rate_limit(&self) {
        self.push_event(UiEvent::ClearRateLimit);
    }
}
