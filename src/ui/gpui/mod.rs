mod elements;
mod input;
mod message;

use crate::ui::{async_trait, DisplayFragment, UIError, UIMessage, UserInterface};
use gpui::{AppContext, Focusable};
use input::TextInput;
use message::MessageView;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use elements::MessageContainer;

// Our main UI struct that implements the UserInterface trait
pub struct GPUI {
    message_queue: Arc<Mutex<Vec<MessageContainer>>>,
    input_value: Arc<Mutex<Option<String>>>,
    input_requested: Arc<Mutex<bool>>,
    ui_update_needed: Arc<Mutex<bool>>,
}

impl GPUI {
    pub fn new() -> Self {
        let message_queue = Arc::new(Mutex::new(Vec::new()));
        let input_value = Arc::new(Mutex::new(None));
        let input_requested = Arc::new(Mutex::new(false));
        let ui_update_needed = Arc::new(Mutex::new(false));

        Self {
            message_queue,
            input_value,
            input_requested,
            ui_update_needed,
        }
    }

    // Run the application
    pub fn run_app(&self) {
        let message_queue = self.message_queue.clone();
        let input_value = self.input_value.clone();
        let input_requested = self.input_requested.clone();
        let ui_update_needed = self.ui_update_needed.clone();

        let app = gpui::Application::new();
        app.run(move |cx| {
            // Register key bindings
            input::register_key_bindings(cx);

            // Create window
            let bounds =
                gpui::Bounds::centered(None, gpui::size(gpui::px(600.0), gpui::px(500.0)), cx);
            let window_result = cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some(gpui::SharedString::from("Code Assistant")),
                        appears_transparent: false,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |_window, cx| {
                    // Create TextInput
                    let text_input = cx.new(|cx| TextInput::new(cx));

                    // Create MessageView with our TextInput
                    cx.new(|cx| {
                        MessageView::new(
                            text_input,
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
        let mut queue = self.message_queue.lock().unwrap();
        if queue.is_empty() {
            let new_message = MessageContainer::new();
            queue.push(new_message.clone());
            new_message
        } else {
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
impl UserInterface for GPUI {
    async fn display(&self, message: UIMessage) -> Result<(), UIError> {
        let mut queue = self.message_queue.lock().unwrap();
        match message {
            UIMessage::Action(msg) | UIMessage::Question(msg) => {
                // Create a new message container with initial text content
                let new_message = MessageContainer::new();
                new_message.add_text_block(msg);
                queue.push(new_message);
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
                message.add_tool_use_block(name, id);
            }
            DisplayFragment::ToolParameter {
                name,
                value,
                tool_id,
            } => {
                message.add_or_update_tool_parameter(tool_id, name, value);
            }
            DisplayFragment::ToolEnd { id } => {
                message.end_tool_use(id);
            }
        }

        // Update the message in the queue
        self.update_message(message);

        Ok(())
    }
}

impl Clone for GPUI {
    fn clone(&self) -> Self {
        Self {
            message_queue: self.message_queue.clone(),
            input_value: self.input_value.clone(),
            input_requested: self.input_requested.clone(),
            ui_update_needed: self.ui_update_needed.clone(),
        }
    }
}
