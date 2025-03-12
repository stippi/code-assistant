use crate::ui::{UIError, UIMessage, UserInterface};
use async_trait::async_trait;
use gpui::{
    div, prelude::*, px, rgb, size, App, Application, Bounds, Context, FocusHandle, SharedString,
    StyleRefinement, Task, Timer, Window, WindowBounds, WindowOptions,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// Main application state
pub struct GPUI {
    app: Arc<Mutex<App>>,
    main_window: Arc<Mutex<Option<Window<AppView>>>>,
    message_queue: Arc<Mutex<Vec<String>>>,
    input_value: Arc<Mutex<Option<String>>>,
    input_requested: Arc<Mutex<bool>>,
}

// Main application view
pub struct AppView {
    message_queue: Arc<Mutex<Vec<String>>>,
    input_value: Arc<Mutex<Option<String>>>,
    input_requested: Arc<Mutex<bool>>,
    focus_handle: FocusHandle,
    input_text: String,
    status_message: String,
    _task: Option<Task<()>>,
}

impl AppView {
    fn new(
        message_queue: Arc<Mutex<Vec<String>>>,
        input_value: Arc<Mutex<Option<String>>>,
        input_requested: Arc<Mutex<bool>>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            message_queue,
            input_value,
            input_requested,
            focus_handle: cx.focus_handle(),
            input_text: String::new(),
            status_message: String::from("Welcome to Code Assistant"),
            _task: None,
        }
    }

    fn handle_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.input_text.trim().is_empty() {
            return;
        }

        // Store input text and flag that input is available
        {
            let mut input_value = self.input_value.lock().unwrap();
            *input_value = Some(self.input_text.clone());
        }

        self.status_message = format!("Submitted: {}", self.input_text);
        self.input_text = String::new();
        cx.notify();

        // Ensure window has focus for continued input
        window.focus_self(cx);
    }

    fn update_input(&mut self, new_text: String, _: &mut Window, cx: &mut Context<Self>) {
        self.input_text = new_text;
        cx.notify();
    }

    fn clear_messages(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let mut messages = self.message_queue.lock().unwrap();
        messages.clear();
        cx.notify();
    }

    fn start_polling_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Clone Arc references for the async task
        let message_queue = self.message_queue.clone();
        let input_requested = self.input_requested.clone();
        let view_entity = cx.entity_id().clone();

        self._task = Some(cx.spawn(|mut cx| async move {
            loop {
                // Check for updates every 100ms
                Timer::after(Duration::from_millis(100)).await;

                let should_update = {
                    let messages = message_queue.lock().unwrap();
                    !messages.is_empty() || *input_requested.lock().unwrap()
                };

                if should_update {
                    let _ = cx.update_window(window, |_, cx| {
                        cx.notify(view_entity);
                    });
                }
            }
        }));
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Start polling task if it doesn't exist
        if self._task.is_none() {
            self.start_polling_task(window, cx);
        }

        // Get the current messages to display
        let messages = {
            let lock = self.message_queue.lock().unwrap();
            lock.clone()
        };

        // Check if input is requested
        let is_input_requested = *self.input_requested.lock().unwrap();

        let input_placeholder = if is_input_requested {
            "Type your response and press Enter..."
        } else {
            "Input disabled while agent is working..."
        };

        div()
            .id("root")
            .size_full()
            .bg(rgb(0x2c2c2c))
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .child(
                // Messages area
                div()
                    .id("messages")
                    .flex_1()
                    .border_1()
                    .border_color(rgb(0x444444))
                    .rounded_md()
                    .p_2()
                    .bg(rgb(0x202020))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(messages.into_iter().map(|msg| {
                        div()
                            .bg(rgb(0x303030))
                            .p_2()
                            .rounded_sm()
                            .whitespace_pre_wrap()
                            .child(msg)
                    })),
            )
            .child(
                // Status bar
                div()
                    .h(px(24.0))
                    .w_full()
                    .bg(rgb(0x333333))
                    .p_1()
                    .rounded_sm()
                    .text_color(rgb(0xcccccc))
                    .text_sm()
                    .child(&self.status_message),
            )
            .child(
                // Input area
                div()
                    .h(px(40.0))
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .bg(rgb(0x303030))
                            .rounded_sm()
                            .border_1()
                            .border_color(if is_input_requested {
                                rgb(0x444444)
                            } else {
                                rgb(0x333333)
                            })
                            .p_2()
                            .text_color(rgb(0xffffff))
                            .hover(|s| {
                                if is_input_requested {
                                    s.border_color(rgb(0x666666))
                                } else {
                                    s
                                }
                            })
                            .active(|s| {
                                if is_input_requested {
                                    s.border_color(rgb(0x888888)).bg(rgb(0x383838))
                                } else {
                                    s
                                }
                            })
                            .track_focus(&self.focus_handle)
                            .on_focus_in(|this, window, cx| {
                                if *this.input_requested.lock().unwrap() {
                                    window.set_cursor_style(gpui::CursorStyle::IBeam);
                                }
                            })
                            .on_focus_out(|_, window, _| {
                                window.set_cursor_style(gpui::CursorStyle::Arrow);
                            })
                            .cursor(if is_input_requested {
                                gpui::CursorStyle::IBeam
                            } else {
                                gpui::CursorStyle::NotAllowed
                            })
                            .on_key_down(move |_, event, _, _| {
                                event.prevent_default();
                                event.stop_propagation();
                            })
                            .on_text_input(move |this, text_input, _, cx| {
                                if *this.input_requested.lock().unwrap() {
                                    let new_text =
                                        format!("{}{}", this.input_text, text_input.text);
                                    this.update_input(new_text, window, cx);
                                }
                            })
                            .on_key_down(move |this, event, _, cx| {
                                if !*this.input_requested.lock().unwrap() {
                                    return;
                                }

                                if event.key_code == gpui::KeyCode::Enter && !event.modifiers.shift
                                {
                                    this.handle_submit(window, cx);
                                    event.prevent_default();
                                } else if event.key_code == gpui::KeyCode::Backspace {
                                    if !this.input_text.is_empty() {
                                        let mut text = this.input_text.clone();
                                        text.pop();
                                        this.update_input(text, window, cx);
                                    }
                                    event.prevent_default();
                                }
                            })
                            .child(if self.input_text.is_empty() {
                                div()
                                    .text_color(rgb(0x777777))
                                    .child(input_placeholder)
                                    .into_any_element()
                            } else {
                                div().child(&self.input_text).into_any_element()
                            }),
                    )
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .bg(rgb(0x444444))
                            .when(is_input_requested, |el| {
                                el.hover(|s| s.bg(rgb(0x666666)))
                                    .active(|s| s.bg(rgb(0x555555)))
                            })
                            .text_color(rgb(0xffffff))
                            .child("Submit")
                            .on_click(move |this, _, window, cx| {
                                if *this.input_requested.lock().unwrap() {
                                    this.handle_submit(window, cx);
                                }
                            }),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .bg(rgb(0x333333))
                            .hover(|s| s.bg(rgb(0x444444)))
                            .active(|s| s.bg(rgb(0x3a3a3a)))
                            .text_color(rgb(0xdddddd))
                            .child("Clear")
                            .on_click(move |this, _, window, cx| {
                                this.clear_messages(window, cx);
                            }),
                    ),
            )
    }
}

impl GPUI {
    pub fn new() -> Self {
        let app = Application::new().into_app();

        // Shared state
        let message_queue = Arc::new(Mutex::new(Vec::new()));
        let input_value = Arc::new(Mutex::new(None::<String>));
        let input_requested = Arc::new(Mutex::new(false));

        let message_queue_clone = message_queue.clone();
        let input_value_clone = input_value.clone();
        let input_requested_clone = input_requested.clone();

        // Create the main window
        let main_window = app.lock().unwrap().run(move |cx| {
            cx.open_window(
                WindowOptions {
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some(SharedString::from("Code Assistant")),
                        appears_transparent: false,
                        ..Default::default()
                    }),
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(800.0), px(600.0)),
                        cx,
                    ))),
                    focus: true,
                    ..Default::default()
                },
                |window, cx| {
                    window.set_input_handler(cx.new_view(|cx| {
                        AppView::new(
                            message_queue_clone.clone(),
                            input_value_clone.clone(),
                            input_requested_clone.clone(),
                            cx,
                        )
                    }))
                },
            )
        });

        Self {
            app: Arc::new(Mutex::new(app.lock().unwrap().clone())),
            main_window: Arc::new(Mutex::new(main_window)),
            message_queue,
            input_value,
            input_requested,
        }
    }

    // Method to process application events - called regularly
    fn update_app(&self) -> Result<(), UIError> {
        let app_clone = self.app.clone();
        let _ = app_clone.lock().unwrap().update(|cx| cx.refresh());
        Ok(())
    }
}

#[async_trait]
impl UserInterface for GPUI {
    async fn display(&self, message: UIMessage) -> Result<(), UIError> {
        match message {
            UIMessage::Action(msg) | UIMessage::Question(msg) => {
                // Add message to queue
                let mut queue = self.message_queue.lock().unwrap();
                queue.push(msg);

                // Update UI
                self.update_app()?;
            }
        }
        Ok(())
    }

    async fn get_input(&self, prompt: &str) -> Result<String, UIError> {
        // Display prompt
        self.display(UIMessage::Question(prompt.to_string()))
            .await?;

        // Set flag to indicate input is requested
        {
            let mut input_requested = self.input_requested.lock().unwrap();
            *input_requested = true;
        }

        // Wait for input
        loop {
            // Check if input is available
            {
                let mut input_value = self.input_value.lock().unwrap();
                if let Some(value) = input_value.take() {
                    // Reset input requested flag
                    let mut input_requested = self.input_requested.lock().unwrap();
                    *input_requested = false;

                    // Return the input
                    return Ok(value);
                }
            }

            // Process UI events
            self.update_app()?;

            // Sleep briefly to avoid busy-waiting
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn display_streaming(&self, text: &str) -> Result<(), UIError> {
        // Add the streaming text to the message queue
        let mut queue = self.message_queue.lock().unwrap();

        // If the queue is empty or if the last message starts with "<streaming>",
        // append to the last message, otherwise add a new message
        if queue.is_empty() {
            queue.push(text.to_string());
        } else {
            let last_index = queue.len() - 1;
            if queue[last_index].starts_with("<streaming>") {
                // Remove the tag and append
                let content = queue[last_index].trim_start_matches("<streaming>");
                queue[last_index] = format!("<streaming>{}{}", content, text);
            } else {
                // Start a new streaming message
                queue.push(format!("<streaming>{}", text));
            }
        }

        // Update UI
        self.update_app()?;

        Ok(())
    }
}
