pub mod assets;
pub mod diff_renderer;
mod elements;
pub mod file_icons;
mod input;
mod memory_view;
mod message;
pub mod parameter_renderers;
mod path_util;
mod scrollbar;
pub mod simple_renderers;

use crate::types::WorkingMemory;
use crate::ui::gpui::{
    diff_renderer::DiffParameterRenderer,
    parameter_renderers::{DefaultParameterRenderer, ParameterRendererRegistry},
    simple_renderers::SimpleParameterRenderer,
};
use crate::ui::{async_trait, DisplayFragment, ToolStatus, UIError, UIMessage, UserInterface};
use gpui::{actions, AppContext, Focusable};
use input::TextInput;
pub use memory_view::MemoryView;
use message::MessageView;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use elements::MessageContainer;

actions!(code_assistant, [CloseWindow]);

// Our main UI struct that implements the UserInterface trait
pub struct Gpui {
    message_queue: Arc<Mutex<Vec<MessageContainer>>>,
    input_value: Arc<Mutex<Option<String>>>,
    input_requested: Arc<Mutex<bool>>,
    ui_update_needed: Arc<Mutex<bool>>,
    working_memory: Arc<Mutex<Option<WorkingMemory>>>,
    current_request_id: Arc<Mutex<u64>>,
    current_tool_counter: Arc<Mutex<u64>>,
    last_xml_tool_id: Arc<Mutex<String>>,
    parameter_renderers: Arc<ParameterRendererRegistry>,
}

impl Gpui {
    pub fn new() -> Self {
        let message_queue = Arc::new(Mutex::new(Vec::new()));
        let input_value = Arc::new(Mutex::new(None));
        let input_requested = Arc::new(Mutex::new(false));
        let ui_update_needed = Arc::new(Mutex::new(false));
        let working_memory = Arc::new(Mutex::new(None));
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

        // Create asset source
        let asset_source = crate::ui::gpui::assets::Assets {};
        // Initialize app with assets
        let app = gpui::Application::new().with_assets(asset_source);

        app.run(move |cx| {
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

            // Register key bindings
            input::register_key_bindings(cx);

            // Create memory view with our shared working memory
            let memory_view = cx.new(|cx| MemoryView::new(working_memory.clone(), cx));

            // Create window with larger size to accommodate both views
            let bounds =
                gpui::Bounds::centered(None, gpui::size(gpui::px(1000.0), gpui::px(650.0)), cx);
            let window_result = cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some(gpui::SharedString::from("Code Assistant")),
                        appears_transparent: true, // Make titlebar transparent
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |_window, cx| {
                    // Create TextInput
                    #[allow(clippy::redundant_closure)]
                    let text_input = cx.new(|cx| TextInput::new(cx));

                    // Create MessageView with our TextInput
                    cx.new(|cx| {
                        MessageView::new(
                            text_input,
                            memory_view.clone(),
                            cx,
                            input_value.clone(),
                            message_queue.clone(),
                            input_requested.clone(),
                        )
                    })
                },
            );

            // Focus the TextInput if window was created successfully
            if let Ok(window_handle) = window_result {
                window_handle
                    .update(cx, |view, window, cx| {
                        window.focus(&view.text_input.focus_handle(cx));
                        cx.activate(true);

                        // Set up the frame refresh cycle
                        Self::setup_frame_refresh_cycle(window, ui_update_needed.clone());
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
            if let Ok(mut flag) = update_flag_ref.lock() {
                if *flag {
                    // Reset the flag
                    *flag = false;
                    updated = true;
                }
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
        // Check if UI update is needed
        let mut updated = false;
        if let Ok(mut flag) = update_flag.lock() {
            if *flag {
                // Reset the flag
                *flag = false;
                updated = true;
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

    // Helper method to get or create a message container
    fn get_or_create_message(&self) -> MessageContainer {
        // Streaming fragments always go to an Assistant message
        self.get_or_create_message_with_role(elements::MessageRole::Assistant)
    }

    // Helper method to get or create a message container with specific role
    fn get_or_create_message_with_role(&self, role: elements::MessageRole) -> MessageContainer {
        let mut queue = self.message_queue.lock().unwrap();
        if queue.is_empty() || queue.last().unwrap().role() != role {
            // If queue is empty or last message has different role, create new message
            let new_message = MessageContainer::with_role(role);
            queue.push(new_message.clone());
            new_message
        } else {
            // Return the existing message with matching role
            queue.last().unwrap().clone()
        }
    }

    // Update a message container in the queue and flag UI for refresh
    fn update_message(&self, message: MessageContainer) {
        // Update the message in the queue
        let mut queue = self.message_queue.lock().unwrap();
        if !queue.is_empty() {
            *queue.last_mut().unwrap() = message;
        } else {
            queue.push(message);
        }

        // Set the flag to indicate that UI refresh is needed
        if let Ok(mut flag) = self.ui_update_needed.lock() {
            *flag = true;
        }
    }
}

#[async_trait]
impl UserInterface for Gpui {
    async fn display(&self, message: UIMessage) -> Result<(), UIError> {
        let mut queue = self.message_queue.lock().unwrap();
        match message {
            UIMessage::Action(msg) | UIMessage::Question(msg) => {
                // For agent actions/questions: Extend current message or create new one
                if let Some(last) = queue.last() {
                    if !last.is_user_message() {
                        // Extend existing assistant message
                        let last = last.clone();
                        last.add_text_block(msg);
                        // Update the message
                        *queue.last_mut().unwrap() = last;

                        // Request UI refresh
                        if let Ok(mut flag) = self.ui_update_needed.lock() {
                            *flag = true;
                        }

                        return Ok(());
                    }
                }

                // Create a new assistant message container
                let new_message =
                    elements::MessageContainer::with_role(elements::MessageRole::Assistant);
                new_message.add_text_block(msg);
                queue.push(new_message);
            }
            UIMessage::UserInput(msg) => {
                // Always create a new container for user input
                let new_message =
                    elements::MessageContainer::with_role(elements::MessageRole::User);
                new_message.add_text_block(msg);
                queue.push(new_message);
            }
        }

        // Request UI refresh
        if let Ok(mut flag) = self.ui_update_needed.lock() {
            *flag = true;
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
        // Get current message or create a new one
        let message = self.get_or_create_message();

        match fragment {
            DisplayFragment::PlainText(text) => {
                message.add_or_append_to_text_block(text);
            }
            DisplayFragment::ThinkingText(text) => {
                message.add_or_append_to_thinking_block(text);
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

                message.add_tool_use_block(name, &tool_id);
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

                message.add_or_update_tool_parameter(&actual_id, name, value);
            }
            DisplayFragment::ToolEnd { id } => {
                // Use last_xml_tool_id if id is empty
                let actual_id = if id.is_empty() {
                    self.last_xml_tool_id.lock().unwrap().clone()
                } else {
                    id.clone()
                };

                message.end_tool_use(&actual_id);
            }
        }

        // Update the message in the queue
        self.update_message(message);

        Ok(())
    }

    async fn update_tool_status(
        &self,
        tool_id: &str,
        status: ToolStatus,
        message: Option<String>,
    ) -> Result<(), UIError> {
        let queue = self.message_queue.lock().unwrap();
        let mut updated = false;

        // Try to update the tool status in all message containers
        for msg_container in queue.iter() {
            if msg_container.update_tool_status(tool_id, status, message.clone()) {
                updated = true;
            }
        }

        if updated {
            // Request UI refresh
            if let Ok(mut flag) = self.ui_update_needed.lock() {
                *flag = true;
            }
        }

        Ok(())
    }

    async fn update_memory(&self, memory: &WorkingMemory) -> Result<(), UIError> {
        // Update the shared working memory directly
        if let Ok(mut memory_guard) = self.working_memory.lock() {
            *memory_guard = Some(memory.clone());
        }

        // Set the update flag to trigger a UI refresh
        if let Ok(mut flag) = self.ui_update_needed.lock() {
            *flag = true;
        }

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
            current_request_id: self.current_request_id.clone(),
            current_tool_counter: self.current_tool_counter.clone(),
            last_xml_tool_id: self.last_xml_tool_id.clone(),
            parameter_renderers: self.parameter_renderers.clone(),
        }
    }
}
