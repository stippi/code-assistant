pub mod assets;
pub mod diff_renderer;
mod elements;
pub mod file_icons;
mod memory;
mod messages;
pub mod parameter_renderers;
mod path_util;
mod root;
pub mod simple_renderers;
pub mod theme;
pub mod ui_events;

use crate::types::WorkingMemory;
use crate::ui::gpui::{
    diff_renderer::DiffParameterRenderer,
    elements::MessageRole,
    parameter_renderers::{DefaultParameterRenderer, ParameterRendererRegistry},
    simple_renderers::SimpleParameterRenderer,
    ui_events::UiEvent,
};
use crate::ui::{async_trait, DisplayFragment, ToolStatus, UIError, UIMessage, UserInterface};
use assets::Assets;
use gpui::{actions, px, AppContext, Entity, Global, Point};
use gpui_component::input::InputState;
use gpui_component::Root;
pub use memory::MemoryView;
pub use messages::MessagesView;
pub use root::RootView;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use elements::MessageContainer;

actions!(code_assistant, [CloseWindow]);

// Our main UI struct that implements the UserInterface trait
pub struct Gpui {
    message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
    input_value: Arc<Mutex<Option<String>>>,
    input_requested: Arc<Mutex<bool>>,
    ui_update_needed: Arc<Mutex<bool>>,
    working_memory: Arc<Mutex<Option<WorkingMemory>>>,
    ui_events: Arc<Mutex<Vec<UiEvent>>>,
    current_request_id: Arc<Mutex<u64>>,
    current_tool_counter: Arc<Mutex<u64>>,
    last_xml_tool_id: Arc<Mutex<String>>,
    parameter_renderers: Arc<ParameterRendererRegistry>,
}

// Implement Global trait for Gpui
impl Global for Gpui {}

impl Gpui {
    pub fn new() -> Self {
        let message_queue = Arc::new(Mutex::new(Vec::new()));
        let input_value = Arc::new(Mutex::new(None));
        let input_requested = Arc::new(Mutex::new(false));
        let ui_update_needed = Arc::new(Mutex::new(false));
        let working_memory = Arc::new(Mutex::new(None));
        let ui_events = Arc::new(Mutex::new(Vec::new()));
        let current_request_id = Arc::new(Mutex::new(0));
        let current_tool_counter = Arc::new(Mutex::new(0));
        let last_xml_tool_id = Arc::new(Mutex::new(String::new()));

        // Initialize parameter renderers registry with default renderer
        let mut registry = ParameterRendererRegistry::new(Box::new(DefaultParameterRenderer));

        // Register specialized renderers
        registry.register_renderer(Box::new(DiffParameterRenderer));

        // Register simple renderers for parameters that don't need labels
        registry.register_renderer(Box::new(SimpleParameterRenderer::new(
            vec![
                ("execute_command".to_string(), "command_line".to_string()),
                ("read_files".to_string(), "paths".to_string()),
                ("list_files".to_string(), "paths".to_string()),
                ("replace_in_file".to_string(), "path".to_string()),
                ("search_files".to_string(), "regex".to_string()),
            ],
            false, // These are not full-width
        )));

        // Wrap the registry in Arc for sharing
        let parameter_renderers = Arc::new(registry);

        // Set the global registry
        ParameterRendererRegistry::set_global(parameter_renderers.clone());

        Self {
            message_queue,
            input_value,
            input_requested,
            ui_update_needed,
            working_memory,
            ui_events,
            current_request_id,
            current_tool_counter,
            last_xml_tool_id,
            parameter_renderers,
        }
    }

    // Run the application
    pub fn run_app(&self) {
        let message_queue = self.message_queue.clone();
        let input_value = self.input_value.clone();
        let input_requested = self.input_requested.clone();
        let ui_update_needed = self.ui_update_needed.clone();
        let working_memory = self.working_memory.clone();
        let gpui_clone = self.clone();

        // Initialize app with assets
        let app = gpui::Application::new().with_assets(Assets {});

        app.run(move |cx| {
            // Register our Gpui instance as a global
            cx.set_global(gpui_clone.clone());

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
            gpui_component::input::init(cx);
            gpui_component::drawer::init(cx);

            // Create memory view with our shared working memory
            let memory_view = cx.new(|cx| MemoryView::new(working_memory.clone(), cx));

            // Create window with larger size to accommodate both views
            let bounds =
                gpui::Bounds::centered(None, gpui::size(gpui::px(1000.0), gpui::px(650.0)), cx);
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
                            // Activate window and set up the frame refresh cycle
                            cx.activate(true);
                            Self::setup_frame_refresh_cycle(window, ui_update_needed.clone());
                        }
                    })
                    .ok();
            }
        });
    }

    // Setup a recurring frame refresh cycle to check for UI updates
    fn setup_frame_refresh_cycle(window: &mut gpui::Window, update_flag: Arc<Mutex<bool>>) {
        // Create a recursive frame handler
        let update_flag_ref = update_flag.clone();
        let frame_handler = move |window: &mut gpui::Window, cx: &mut gpui::App| {
            // Check if UI update is needed
            let mut updated = false;
            let mut flag = update_flag_ref.lock().unwrap();
            if *flag {
                // Reset the flag
                *flag = false;
                updated = true;
            }

            // If updates were requested, refresh the window
            if updated {
                cx.refresh_windows();
            }

            // Schedule another check for the next frame by creating a new closure
            // that captures our update_flag
            let new_handler = {
                let update_flag = update_flag_ref.clone();
                move |window: &mut gpui::Window, cx: &mut gpui::App| {
                    Self::handle_frame(window, cx, update_flag);
                }
            };

            window.on_next_frame(new_handler);
        };

        // Start the refresh cycle
        window.on_next_frame(frame_handler);
    }

    // Helper method for the recurring frame handler
    fn handle_frame(window: &mut gpui::Window, cx: &mut gpui::App, update_flag: Arc<Mutex<bool>>) {
        let mut updated = false;

        // Check update flag
        let mut flag = update_flag.lock().unwrap();
        if *flag {
            // Reset the flag
            *flag = false;
            updated = true;
        }

        // Get a clone of the global Gpui
        let gpui = cx.global::<Gpui>().clone();

        // Process any pending UI events in the queue
        let events = {
            let mut events_queue = gpui.ui_events.lock().unwrap();
            if !events_queue.is_empty() {
                updated = true;
                std::mem::take(&mut *events_queue)
            } else {
                Vec::new()
            }
        };

        // Process each event
        if !events.is_empty() {
            for event in events {
                gpui.process_ui_event(event, window, cx);
            }
        }

        // If updates were requested, refresh the window
        if updated {
            cx.refresh_windows();
        }

        // Schedule another check for the next frame
        let new_handler = {
            let update_flag = update_flag.clone();
            move |window: &mut gpui::Window, cx: &mut gpui::App| {
                Self::handle_frame(window, cx, update_flag);
            }
        };

        window.on_next_frame(new_handler);
    }

    // Process a UI event in the UI thread context
    fn process_ui_event(&self, event: UiEvent, _window: &mut gpui::Window, cx: &mut gpui::App) {
        match event {
            UiEvent::DisplayMessage { content, role } => {
                let mut queue = self.message_queue.lock().unwrap();
                let new_message = cx.new(|cx| {
                    let new_message = MessageContainer::with_role(role, cx);
                    new_message.add_text_block(&content, cx);
                    new_message
                });
                queue.push(new_message);
            }
            UiEvent::AppendToTextBlock { content } => {
                let mut queue = self.message_queue.lock().unwrap();
                if let Some(last) = queue.last() {
                    // Check if the last message is from the assistant, otherwise create a new one
                    let is_user_message =
                        cx.update_entity(&last, |message, _cx| message.is_user_message());

                    if is_user_message {
                        // Create a new assistant message
                        let new_message = cx.new(|cx| {
                            let new_message =
                                MessageContainer::with_role(MessageRole::Assistant, cx);
                            new_message.add_text_block(&content, cx);
                            new_message
                        });
                        queue.push(new_message);
                    } else {
                        // Update the existing assistant message
                        cx.update_entity(&last, |message, cx| {
                            message.add_or_append_to_text_block(&content, cx)
                        });
                    }
                } else {
                    // If there are no messages, create a new assistant message
                    let new_message = cx.new(|cx| {
                        let new_message = MessageContainer::with_role(MessageRole::Assistant, cx);
                        new_message.add_text_block(&content, cx);
                        new_message
                    });
                    queue.push(new_message);
                }
            }
            UiEvent::AppendToThinkingBlock { content } => {
                let mut queue = self.message_queue.lock().unwrap();
                if let Some(last) = queue.last() {
                    // Check if the last message is from the assistant, otherwise create a new one
                    let is_user_message =
                        cx.update_entity(&last, |message, _cx| message.is_user_message());

                    if is_user_message {
                        // Create a new assistant message
                        let new_message = cx.new(|cx| {
                            let new_message =
                                MessageContainer::with_role(MessageRole::Assistant, cx);
                            new_message.add_thinking_block(&content, cx);
                            new_message
                        });
                        queue.push(new_message);
                    } else {
                        // Update the existing assistant message
                        cx.update_entity(&last, |message, cx| {
                            message.add_or_append_to_thinking_block(&content, cx)
                        });
                    }
                } else {
                    // If there are no messages, create a new assistant message
                    let new_message = cx.new(|cx| {
                        let new_message = MessageContainer::with_role(MessageRole::Assistant, cx);
                        new_message.add_thinking_block(&content, cx);
                        new_message
                    });
                    queue.push(new_message);
                }
            }
            UiEvent::StartTool { name, id } => {
                let mut queue = self.message_queue.lock().unwrap();
                if let Some(last) = queue.last() {
                    cx.update_entity(&last, |message, cx| {
                        message.add_tool_use_block(&name, &id, cx);
                    });
                } else {
                    // Create a new assistant message if none exists
                    let new_message = cx.new(|cx| {
                        let new_message = MessageContainer::with_role(MessageRole::Assistant, cx);
                        new_message.add_tool_use_block(&name, &id, cx);
                        new_message
                    });
                    queue.push(new_message);
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
                    });
                }
            }
            UiEvent::UpdateToolStatus {
                tool_id,
                status,
                message,
            } => {
                let queue = self.message_queue.lock().unwrap();
                for message_container in queue.iter() {
                    cx.update_entity(&message_container, |message_container, cx| {
                        message_container.update_tool_status(&tool_id, status, message.clone(), cx);
                    });
                }
            }
            UiEvent::EndTool { id } => {
                let queue = self.message_queue.lock().unwrap();
                for message_container in queue.iter() {
                    cx.update_entity(&message_container, |message_container, cx| {
                        message_container.end_tool_use(&id, cx);
                    });
                }
            }
        }
    }

    // Helper to add an event to the queue
    fn push_event(&self, event: UiEvent) {
        let mut events = self.ui_events.lock().unwrap();
        events.push(event);

        // Set the update flag to trigger a refresh
        let mut flag = self.ui_update_needed.lock().unwrap();
        *flag = true;
    }
}

#[async_trait]
impl UserInterface for Gpui {
    async fn display(&self, message: UIMessage) -> Result<(), UIError> {
        match message {
            UIMessage::Action(msg) | UIMessage::Question(msg) => {
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
        }

        Ok(())
    }

    async fn get_input(&self, prompt: &str) -> Result<String, UIError> {
        // Display prompt
        self.display(UIMessage::Question(prompt.to_string()))
            .await?;

        // Request input
        {
            let mut requested = self.input_requested.lock().unwrap();
            *requested = true;
        }

        // Wait for input
        loop {
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
    ) -> Result<(), UIError> {
        // Push an event to update tool status
        self.push_event(UiEvent::UpdateToolStatus {
            tool_id: tool_id.to_string(),
            status,
            message,
        });

        Ok(())
    }

    async fn update_memory(&self, memory: &WorkingMemory) -> Result<(), UIError> {
        // Update the shared working memory directly
        if let Ok(mut memory_guard) = self.working_memory.lock() {
            *memory_guard = Some(memory.clone());
        }

        // Set the update flag to trigger a UI refresh
        let mut flag = self.ui_update_needed.lock().unwrap();
        *flag = true;

        Ok(())
    }

    async fn begin_llm_request(&self) -> Result<u64, UIError> {
        // Increment request ID counter
        let mut request_id = self.current_request_id.lock().unwrap();
        *request_id += 1;

        // Reset tool counter for this request
        let mut tool_counter = self.current_tool_counter.lock().unwrap();
        *tool_counter = 0;

        Ok(*request_id)
    }

    async fn end_llm_request(&self, _request_id: u64) -> Result<(), UIError> {
        // For now, we don't need special handling for request completion
        Ok(())
    }
}

impl Clone for Gpui {
    fn clone(&self) -> Self {
        Self {
            message_queue: self.message_queue.clone(),
            input_value: self.input_value.clone(),
            input_requested: self.input_requested.clone(),
            ui_update_needed: self.ui_update_needed.clone(),
            working_memory: self.working_memory.clone(),
            ui_events: self.ui_events.clone(),
            current_request_id: self.current_request_id.clone(),
            current_tool_counter: self.current_tool_counter.clone(),
            last_xml_tool_id: self.last_xml_tool_id.clone(),
            parameter_renderers: self.parameter_renderers.clone(),
        }
    }
}
