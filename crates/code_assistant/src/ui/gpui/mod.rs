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
pub mod ui_events;

use crate::persistence::ChatMetadata;
use crate::types::WorkingMemory;
use crate::ui::gpui::ui_events::MessageData;
use crate::ui::gpui::{
    content_renderer::ContentRenderer,
    diff_renderer::DiffParameterRenderer,
    elements::MessageRole,
    parameter_renderers::{DefaultParameterRenderer, ParameterRendererRegistry},
    simple_renderers::SimpleParameterRenderer,
    ui_events::UiEvent,
};
use crate::ui::{
    async_trait, DisplayFragment, StreamingState, ToolStatus, UIError, UIMessage, UserInterface,
};
use llm;
use assets::Assets;
use async_channel;
use gpui::{actions, px, AppContext, AsyncApp, Entity, Global, Point};
use gpui_component::input::InputState;
use gpui_component::Root;
pub use memory::MemoryView;
pub use messages::MessagesView;
pub use root::RootView;


use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::warn;

use elements::MessageContainer;

actions!(code_assistant, [CloseWindow]);

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
    SessionLoaded { session_id: String, messages: Vec<llm::Message> },
    SessionCreated { session_id: String, name: String },
    SessionDeleted { session_id: String },
    SessionsListed { sessions: Vec<ChatMetadata> },
    Error { message: String },
}

// Legacy aliases for compatibility during transition
pub type ChatManagementEvent = BackendEvent;
pub type ChatManagementResponse = BackendResponse;

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
    current_tool_counter: Arc<Mutex<u64>>,
    last_xml_tool_id: Arc<Mutex<String>>,
    #[allow(dead_code)]
    parameter_renderers: Arc<ParameterRendererRegistry>, // TODO: Needed?!
    streaming_state: Arc<Mutex<StreamingState>>,
    // Unified backend communication
    backend_event_sender: Arc<Mutex<Option<async_channel::Sender<BackendEvent>>>>,
    backend_response_receiver: Arc<Mutex<Option<async_channel::Receiver<BackendResponse>>>>,

    // Current chat state
    current_session_id: Arc<Mutex<Option<String>>>,
    chat_sessions: Arc<Mutex<Vec<ChatMetadata>>>,
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
        let current_tool_counter = Arc::new(Mutex::new(0));
        let last_xml_tool_id = Arc::new(Mutex::new(String::new()));
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
            current_tool_counter,
            last_xml_tool_id,
            parameter_renderers,
            streaming_state,
            backend_event_sender: Arc::new(Mutex::new(None)),
            backend_response_receiver: Arc::new(Mutex::new(None)),

            current_session_id: Arc::new(Mutex::new(None)),
            chat_sessions: Arc::new(Mutex::new(Vec::new())),
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
            theme::init_themes(cx);
            gpui_component::init(cx);

            // Spawn task to receive UiEvents
            let rx = gpui_clone.event_receiver.lock().unwrap().clone();
            let async_gpui_clone = gpui_clone.clone();
            tracing::info!("Starting UI event processing task");
            let task = cx.spawn(async move |cx: &mut AsyncApp| {
                tracing::info!("UI event processing task is running");
                loop {
                    tracing::debug!("Waiting for UI event...");
                    let result = rx.recv().await;
                    match result {
                        Ok(received_event) => {
                            tracing::info!("UI event processing: Received event: {:?}", received_event);
                            async_gpui_clone.process_ui_event_async(received_event, cx);
                        }
                        Err(err) => {
                            warn!("Receive error: {}", err);
                            break;
                        }
                    }
                }
                tracing::warn!("UI event processing task ended");
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
            let window_result = cx.open_window(
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
                    let messages_view = cx.new(|cx| MessagesView::new(message_queue.clone(), cx));

                    // Create RootView
                    let root_view = cx.new(|cx| {
                        RootView::new(
                            text_input,
                            memory_view.clone(),
                            messages_view,
                            cx,
                            input_value.clone(),
                            input_requested.clone(),
                            gpui_clone.streaming_state.clone(),
                        )
                    });

                    // Wrap in Root component
                    cx.new(|cx| Root::new(root_view.into(), window, cx))
                },
            );

            // Focus the TextInput if window was created successfully
            if let Ok(window_handle) = window_result {
                window_handle
                    .update(cx, |_root, window, cx| {
                        // Get the MessageView from the Root
                        if let Some(_view) =
                            window.root::<gpui_component::Root>().and_then(|root| root)
                        {
                            // Activate window
                            cx.activate(true);
                        }
                    })
                    .ok();
            }
        });
    }

    fn process_ui_event_async(&self, event: UiEvent, cx: &mut gpui::AsyncApp) {
        match event {
            UiEvent::DisplayMessage { content, role } => {
                let mut queue = self.message_queue.lock().unwrap();
                let result = cx.new(|cx| {
                    let new_message = MessageContainer::with_role(role, cx);
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
            UiEvent::SetMessages { messages, session_id } => {
                // Update current session ID if provided
                if let Some(session_id) = session_id {
                    *self.current_session_id.lock().unwrap() = Some(session_id);
                }

                // Clear existing messages
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    queue.clear();
                }

                // Create new message containers from the message data
                for message_data in messages {
                    let container = cx.new(|cx| MessageContainer::with_role(message_data.role, cx))
                        .expect("Failed to create message container");

                    // Process all fragments for this message
                    self.process_fragments_for_container(&container, message_data.fragments, cx);

                    // Add container to queue
                    {
                        let mut queue = self.message_queue.lock().unwrap();
                        queue.push(container);
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
                tracing::info!("UI: LoadChatSession event for session_id: {}", session_id);
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::LoadSession { session_id });
                }
            }
            UiEvent::CreateNewChatSession { name } => {
                tracing::info!("UI: CreateNewChatSession event with name: {:?}", name);
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::CreateNewSession { name });
                }
            }
            UiEvent::DeleteChatSession { session_id } => {
                tracing::info!("UI: DeleteChatSession event for session_id: {}", session_id);
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::DeleteSession { session_id });
                }
            }
            UiEvent::RefreshChatList => {
                tracing::info!("UI: RefreshChatList event received");
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    tracing::info!("UI: Sending ListSessions to backend");
                    let _ = sender.try_send(BackendEvent::ListSessions);
                } else {
                    tracing::warn!("UI: No backend event sender available for RefreshChatList");
                }
            }
            UiEvent::UpdateChatList { sessions } => {
                tracing::info!("UI: UpdateChatList event received with {} sessions", sessions.len());
                // Update local cache
                *self.chat_sessions.lock().unwrap() = sessions.clone();
                let _current_session_id = self.current_session_id.lock().unwrap().clone();

                // Refresh all windows to trigger re-render with new chat data
                tracing::info!("UI: Refreshing windows for chat list update");
                cx.refresh().expect("Failed to refresh windows");
            }
            // New v2 architecture events
            UiEvent::LoadSessionFragments { fragments, session_id } => {
                tracing::info!("UI: LoadSessionFragments event for session {}", session_id);

                // Set as active session
                *self.current_session_id.lock().unwrap() = Some(session_id);

                // Clear existing messages
                {
                    let mut queue = self.message_queue.lock().unwrap();
                    queue.clear();
                }

                // Create a single assistant container for all fragments
                if !fragments.is_empty() {
                    let container = cx.new(|cx| MessageContainer::with_role(MessageRole::Assistant, cx))
                        .expect("Failed to create message container");

                    // Process all fragments
                    self.process_fragments_for_container(&container, fragments, cx);

                    // Add to queue
                    {
                        let mut queue = self.message_queue.lock().unwrap();
                        queue.push(container);
                    }
                }

                cx.refresh().expect("Failed to refresh windows");
            }
            UiEvent::ClearMessages => {
                tracing::info!("UI: ClearMessages event");
                let mut queue = self.message_queue.lock().unwrap();
                queue.clear();
                cx.refresh().expect("Failed to refresh windows");
            }
            UiEvent::SendUserMessage { message, session_id } => {
                tracing::info!("UI: SendUserMessage event for session {}: {}", session_id, message);
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::SendUserMessage { session_id, message });
                } else {
                    tracing::warn!("UI: No backend event sender available");
                }
            }
            UiEvent::ConnectToActiveSession { session_id } => {
                tracing::info!("UI: ConnectToActiveSession event for session {}", session_id);
                // Set as active session
                *self.current_session_id.lock().unwrap() = Some(session_id.clone());

                // Request buffered fragments from MultiSessionManager
                // This would need to be implemented in the backend
                tracing::info!("UI: TODO - Request buffered fragments for session {}", session_id);
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

    // Setup chat management communication channels
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

    // Legacy methods for compatibility during transition
    pub fn setup_chat_communication(
        &self,
    ) -> (
        async_channel::Receiver<ChatManagementEvent>,
        async_channel::Sender<ChatManagementResponse>,
    ) {
        self.setup_backend_communication()
    }

    pub fn setup_v2_communication(
        &self,
        _user_message_tx: async_channel::Sender<(String, String)>,
        session_event_tx: async_channel::Sender<ChatManagementEvent>,
        session_response_rx: async_channel::Receiver<ChatManagementResponse>,
    ) {
        // For backward compatibility, but now we ignore the separate user_message_tx
        *self.backend_event_sender.lock().unwrap() = Some(session_event_tx);
        *self.backend_response_receiver.lock().unwrap() = Some(session_response_rx);
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

    // Send a user message to the active session
    pub fn send_user_message_to_active_session(&self, message: String) -> Result<(), String> {
        let session_id = self.get_current_session_id()
            .ok_or_else(|| "No active session".to_string())?;

        let sender = self.backend_event_sender.lock().unwrap();
        if let Some(ref tx) = *sender {
            tx.try_send(BackendEvent::SendUserMessage { session_id, message })
                .map_err(|e| format!("Failed to send message: {}", e))?;
            Ok(())
        } else {
            Err("Backend event sender not initialized".to_string())
        }
    }

    // Handle backend responses
    fn handle_backend_response(&self, response: BackendResponse, _cx: &mut AsyncApp) {
        tracing::info!("UI: Received chat management response: {:?}", response);
        match response {
            BackendResponse::SessionLoaded { session_id, messages } => {
                *self.current_session_id.lock().unwrap() = Some(session_id.clone());

                // Create fragments from loaded messages and send SetMessages event
                match self.create_fragments_from_messages(&messages) {
                    Ok(message_data) => {
                        tracing::info!("Created {} message containers from loaded session", message_data.len());
                        self.push_event(UiEvent::SetMessages {
                            messages: message_data,
                            session_id: Some(session_id)
                        });
                    }
                    Err(e) => {
                        tracing::error!("Failed to create fragments from messages: {}", e);
                        // Fallback: just clear messages
                        self.message_queue.lock().unwrap().clear();
                    }
                }
            }
            BackendResponse::SessionCreated {
                session_id,
                name: _,
            } => {
                *self.current_session_id.lock().unwrap() = Some(session_id);
                // Refresh the session list
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::ListSessions);
                }
            }
            BackendResponse::SessionDeleted { session_id: _ } => {
                // Refresh the session list
                if let Some(sender) = self.backend_event_sender.lock().unwrap().as_ref() {
                    let _ = sender.try_send(BackendEvent::ListSessions);
                }
            }
            BackendResponse::SessionsListed { sessions } => {
                *self.chat_sessions.lock().unwrap() = sessions.clone();
                self.push_event(UiEvent::UpdateChatList { sessions });
            }
            BackendResponse::Error { message } => {
                warn!("Backend error: {}", message);
            }
        }
    }

    // Update chat state from agent responses
    pub fn update_chat_state(&self, session_id: Option<String>, sessions: Vec<ChatMetadata>) {
        *self.current_session_id.lock().unwrap() = session_id;
        *self.chat_sessions.lock().unwrap() = sessions;

        // Trigger UI update
        self.push_event(UiEvent::UpdateChatList {
            sessions: self.chat_sessions.lock().unwrap().clone(),
        });
    }


}

impl Gpui {
    // Create message data from loaded session messages using StreamProcessor
    fn create_fragments_from_messages(&self, messages: &[llm::Message]) -> Result<Vec<MessageData>, UIError> {
        use crate::ui::streaming::create_stream_processor;

        // Create dummy UI for stream processor (same as in Agent)
        struct DummyUI;
        #[async_trait::async_trait]
        impl crate::ui::UserInterface for DummyUI {
            async fn display(&self, _message: crate::ui::UIMessage) -> Result<(), crate::ui::UIError> { Ok(()) }
            async fn get_input(&self) -> Result<String, crate::ui::UIError> { Ok("".to_string()) }
            fn display_fragment(&self, _fragment: &crate::ui::DisplayFragment) -> Result<(), crate::ui::UIError> { Ok(()) }
            async fn update_memory(&self, _memory: &crate::types::WorkingMemory) -> Result<(), crate::ui::UIError> { Ok(()) }
            async fn update_tool_status(&self, _tool_id: &str, _status: crate::ui::ToolStatus, _message: Option<String>, _output: Option<String>) -> Result<(), crate::ui::UIError> { Ok(()) }
            async fn begin_llm_request(&self) -> Result<u64, crate::ui::UIError> { Ok(0) }
            async fn end_llm_request(&self, _request_id: u64, _cancelled: bool) -> Result<(), crate::ui::UIError> { Ok(()) }
            fn should_streaming_continue(&self) -> bool { true }
        }

        let dummy_ui = std::sync::Arc::new(Box::new(DummyUI) as Box<dyn crate::ui::UserInterface>);

        // Use Native mode as default - TODO: Get actual tool mode from somewhere
        let mut processor = create_stream_processor(crate::types::ToolMode::Native, dummy_ui);

        let mut messages_data = Vec::new();
        tracing::info!("Processing {} messages for UI fragments", messages.len());

        for (i, message) in messages.iter().enumerate() {
            match processor.extract_fragments_from_message(message) {
                Ok(fragments) => {
                    let role = match message.role {
                        llm::MessageRole::User => MessageRole::User,
                        llm::MessageRole::Assistant => MessageRole::Assistant,
                    };
                    tracing::info!("Message {}: Extracted {} fragments", i, fragments.len());
                    messages_data.push(MessageData { role, fragments });
                }
                Err(e) => {
                    tracing::error!("Message {}: Failed to extract fragments: {}", i, e);
                }
            }
        }

        tracing::info!("Created {} message containers", messages_data.len());
        Ok(messages_data)
    }
}

#[async_trait]
impl UserInterface for Gpui {
    async fn display(&self, message: UIMessage) -> Result<(), UIError> {
        match message {
            UIMessage::Action(msg) => {
                // Create a new assistant message
                self.push_event(UiEvent::DisplayMessage {
                    content: msg,
                    role: MessageRole::Assistant,
                });
            }
            UIMessage::UserInput(msg) => {
                // Always create a new container for user input
                self.push_event(UiEvent::DisplayMessage {
                    content: msg,
                    role: MessageRole::User,
                });
            }
            UIMessage::UiEvent(event) => {
                // Forward UI events directly to the event processing
                self.push_event(event);
            }
        }

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
                let tool_id = if id.is_empty() {
                    // XML case: Generate ID based on request ID and tool counter
                    let request_id = *self.current_request_id.lock().unwrap();
                    let mut tool_counter = self.current_tool_counter.lock().unwrap();
                    *tool_counter += 1;
                    let new_id = format!("tool-{}-{}", request_id, tool_counter);

                    // Save this ID for subsequent empty parameter IDs
                    *self.last_xml_tool_id.lock().unwrap() = new_id.clone();

                    new_id
                } else {
                    // JSON case: Use the provided ID
                    id.clone()
                };

                self.push_event(UiEvent::StartTool {
                    name: name.clone(),
                    id: tool_id,
                });
            }
            DisplayFragment::ToolParameter {
                name,
                value,
                tool_id,
            } => {
                // Use last_xml_tool_id if tool_id is empty
                let actual_id = if tool_id.is_empty() {
                    self.last_xml_tool_id.lock().unwrap().clone()
                } else {
                    tool_id.clone()
                };

                self.push_event(UiEvent::UpdateToolParameter {
                    tool_id: actual_id,
                    name: name.clone(),
                    value: value.clone(),
                });
            }
            DisplayFragment::ToolEnd { id } => {
                // Use last_xml_tool_id if id is empty
                let tool_id = if id.is_empty() {
                    self.last_xml_tool_id.lock().unwrap().clone()
                } else {
                    id.clone()
                };

                self.push_event(UiEvent::EndTool { id: tool_id });
            }
        }

        Ok(())
    }

    async fn update_tool_status(
        &self,
        tool_id: &str,
        status: ToolStatus,
        message: Option<String>,
        output: Option<String>,
    ) -> Result<(), UIError> {
        // Push an event to update tool status
        self.push_event(UiEvent::UpdateToolStatus {
            tool_id: tool_id.to_string(),
            status,
            message,
            output,
        });

        Ok(())
    }

    async fn update_memory(&self, memory: &WorkingMemory) -> Result<(), UIError> {
        // Push an event to update working memory
        self.push_event(UiEvent::UpdateMemory {
            memory: memory.clone(),
        });
        Ok(())
    }

    async fn begin_llm_request(&self) -> Result<u64, UIError> {
        // Set streaming state to Streaming
        *self.streaming_state.lock().unwrap() = StreamingState::Streaming;

        // Increment request ID counter
        let mut request_id = self.current_request_id.lock().unwrap();
        *request_id += 1;
        let current_id = *request_id;

        // Reset tool counter for this request
        let mut tool_counter = self.current_tool_counter.lock().unwrap();
        *tool_counter = 0;

        // Send StreamingStarted event
        self.push_event(UiEvent::StreamingStarted(current_id));

        Ok(current_id)
    }

    async fn end_llm_request(&self, request_id: u64, cancelled: bool) -> Result<(), UIError> {
        // Reset streaming state to Idle
        *self.streaming_state.lock().unwrap() = StreamingState::Idle;

        // Send StreamingStopped event
        self.push_event(UiEvent::StreamingStopped {
            id: request_id,
            cancelled,
        });

        Ok(())
    }

    fn should_streaming_continue(&self) -> bool {
        match *self.streaming_state.lock().unwrap() {
            StreamingState::StopRequested => false,
            _ => true,
        }
    }
}
