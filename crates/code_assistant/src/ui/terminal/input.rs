use std::collections::HashMap;

use base64::Engine;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tracing::debug;

use crate::persistence::DraftAttachment;

use super::commands::{CommandProcessor, CommandResult};
use super::textarea::TextArea;

/// Threshold in characters above which pasted text is collapsed into a placeholder.
const LARGE_PASTE_CHAR_THRESHOLD: usize = 200;

/// Result of handling a key event
#[derive(Debug)]
pub enum KeyEventResult {
    /// Continue processing normally
    Continue,
    /// Quit the application
    Quit,
    /// Submit a message with optional attachments
    SendMessage {
        message: String,
        attachments: Vec<DraftAttachment>,
    },
    /// Escape key was pressed - main loop decides what to do
    Escape,
    /// Display information message
    ShowInfo(String),
    /// Switch to a different model
    SwitchModel(String),
    /// Show current model information
    ShowCurrentModel,
    /// Toggle plan rendering mode
    TogglePlan,
}

/// Manages the input area using the custom TextArea widget
pub struct InputManager {
    pub textarea: TextArea,
    command_processor: Option<CommandProcessor>,
    /// Attachments accumulated from paste operations (images).
    pub attachments: Vec<DraftAttachment>,
    /// Counter for image paste placeholders.
    image_counter: usize,
    /// Map from placeholder text to the actual pasted content (for large text pastes).
    pending_pastes: Vec<(String, String)>,
    /// Counters for generating unique large-paste placeholders (keyed by char_count).
    large_paste_counters: HashMap<usize, usize>,
}

impl InputManager {
    pub fn new() -> Self {
        let command_processor = CommandProcessor::new().ok();
        Self {
            textarea: TextArea::new(),
            command_processor,
            attachments: Vec::new(),
            image_counter: 0,
            pending_pastes: Vec::new(),
            large_paste_counters: HashMap::new(),
        }
    }

    /// Handle a key event and return the appropriate result
    pub fn handle_key_event(&mut self, key_event: KeyEvent) -> KeyEventResult {
        match key_event {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => KeyEventResult::Quit,
            // Ctrl-V / Alt-V: try to paste an image from clipboard.
            // On macOS, Cmd-V is handled by the terminal and produces Event::Paste for text.
            // Ctrl-V lets users explicitly paste clipboard images (which don't produce Paste events).
            KeyEvent {
                code: KeyCode::Char('v'),
                modifiers,
                ..
            } if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                if !self.try_paste_clipboard_image() {
                    debug!("No clipboard image found on Ctrl/Alt-V");
                }
                KeyEventResult::Continue
            }
            KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::NONE,
                ..
            } => KeyEventResult::Escape,
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::SHIFT,
                ..
            } => {
                self.textarea.insert_str("\n");
                KeyEventResult::Continue
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                // Submit input
                let content = self.build_submit_content();
                if !content.is_empty() {
                    // Take attachments before clearing, so they're not lost.
                    let attachments = self.take_attachments();
                    self.clear();

                    // Check if this is a slash command
                    if let Some(ref processor) = self.command_processor {
                        match processor.process_command(&content) {
                            CommandResult::Continue => KeyEventResult::SendMessage {
                                message: content,
                                attachments,
                            },
                            CommandResult::Help(help_text) => KeyEventResult::ShowInfo(help_text),
                            CommandResult::ListModels => {
                                KeyEventResult::ShowInfo(processor.get_models_list())
                            }
                            CommandResult::ListProviders => {
                                KeyEventResult::ShowInfo(processor.get_providers_list())
                            }
                            CommandResult::SwitchModel(model_name) => {
                                KeyEventResult::SwitchModel(model_name)
                            }
                            CommandResult::ShowCurrentModel => KeyEventResult::ShowCurrentModel,
                            CommandResult::TogglePlan => KeyEventResult::TogglePlan,
                            CommandResult::InvalidCommand(error) => {
                                KeyEventResult::ShowInfo(format!("Error: {error}"))
                            }
                        }
                    } else {
                        // Command processor not available, treat as regular message
                        KeyEventResult::SendMessage {
                            message: content,
                            attachments,
                        }
                    }
                } else {
                    KeyEventResult::Continue
                }
            }
            _ => {
                // Forward the key event directly to our custom TextArea
                self.textarea.input(key_event);
                KeyEventResult::Continue
            }
        }
    }

    /// Handle a terminal paste event (from bracketed paste).
    pub fn handle_paste(&mut self, pasted: String) {
        let pasted = pasted.replace("\r\n", "\n").replace('\r', "\n");
        let char_count = pasted.chars().count();

        if char_count > LARGE_PASTE_CHAR_THRESHOLD {
            let line_count = pasted.lines().count();
            let placeholder = self.next_large_paste_placeholder(line_count);
            self.textarea.insert_element(&placeholder);
            self.pending_pastes.push((placeholder, pasted));
        } else {
            self.textarea.insert_str(&pasted);
        }
    }

    /// Try to read an image from the system clipboard and attach it.
    /// Returns true if an image was found and attached.
    pub fn try_paste_clipboard_image(&mut self) -> bool {
        let Ok(mut clipboard) = arboard::Clipboard::new() else {
            return false;
        };

        // Try to get image data from clipboard
        match clipboard.get_image() {
            Ok(img_data) => {
                let w = img_data.width as u32;
                let h = img_data.height as u32;
                debug!("Clipboard image: {}x{}", w, h);

                // Convert to PNG
                let Some(rgba_img) = image::RgbaImage::from_raw(w, h, img_data.bytes.into_owned())
                else {
                    debug!("Failed to create RGBA image from clipboard data");
                    return false;
                };

                let dyn_img = image::DynamicImage::ImageRgba8(rgba_img);
                let mut png_bytes: Vec<u8> = Vec::new();
                let mut cursor = std::io::Cursor::new(&mut png_bytes);
                if dyn_img
                    .write_to(&mut cursor, image::ImageFormat::Png)
                    .is_err()
                {
                    debug!("Failed to encode clipboard image as PNG");
                    return false;
                }

                let base64_content = base64::engine::general_purpose::STANDARD.encode(&png_bytes);

                self.image_counter += 1;
                let placeholder = format!("[Image {}]", self.image_counter);

                self.attachments.push(DraftAttachment::Image {
                    content: base64_content,
                    mime_type: "image/png".to_string(),
                    width: Some(w),
                    height: Some(h),
                });

                self.textarea.insert_element(&placeholder);
                debug!("Attached clipboard image as {}", placeholder);
                true
            }
            Err(_) => false,
        }
    }

    /// Build the final message content, expanding large-paste placeholders.
    fn build_submit_content(&self) -> String {
        let raw = self.textarea.text().to_string();
        if self.pending_pastes.is_empty() {
            return raw;
        }

        let mut result = raw;
        for (placeholder, content) in &self.pending_pastes {
            result = result.replace(placeholder, content);
        }
        result
    }

    /// Take the accumulated attachments, leaving the internal list empty.
    pub fn take_attachments(&mut self) -> Vec<DraftAttachment> {
        std::mem::take(&mut self.attachments)
    }

    /// Clear the textarea content and all paste state.
    pub fn clear(&mut self) {
        self.textarea.clear();
        self.attachments.clear();
        self.image_counter = 0;
        self.pending_pastes.clear();
        self.large_paste_counters.clear();
    }

    fn next_large_paste_placeholder(&mut self, line_count: usize) -> String {
        let counter = self.large_paste_counters.entry(line_count).or_insert(0);
        *counter += 1;
        if *counter == 1 {
            format!("[Pasted {} lines]", line_count)
        } else {
            format!("[Pasted {} lines] #{}", line_count, counter)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn create_key_event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn test_input_manager_basic_operations() {
        let mut input_manager = InputManager::new();

        // Test initial state
        assert_eq!(input_manager.textarea.text(), "");

        // Test character input
        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('h'), KeyModifiers::NONE));
        assert!(matches!(result, KeyEventResult::Continue));

        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('i'), KeyModifiers::NONE));
        assert!(matches!(result, KeyEventResult::Continue));

        // Content should contain the typed characters
        let content = input_manager.textarea.text();
        assert_eq!(content, "hi");

        // Test submission
        let result =
            input_manager.handle_key_event(create_key_event(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            KeyEventResult::SendMessage {
                message,
                attachments,
            } => {
                assert_eq!(message, "hi");
                assert!(attachments.is_empty());
            }
            other => panic!("Expected SendMessage, got {:?}", other),
        }

        // Content should be cleared after submission
        assert_eq!(input_manager.textarea.text(), "");
    }

    #[test]
    fn test_quit_signal() {
        let mut input_manager = InputManager::new();

        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(matches!(result, KeyEventResult::Quit));
    }

    #[test]
    fn test_escape_key() {
        let mut input_manager = InputManager::new();

        let result =
            input_manager.handle_key_event(create_key_event(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(result, KeyEventResult::Escape));
    }

    #[test]
    fn test_newline_handling() {
        let mut input_manager = InputManager::new();

        // Type some text
        input_manager.handle_key_event(create_key_event(KeyCode::Char('h'), KeyModifiers::NONE));
        input_manager.handle_key_event(create_key_event(KeyCode::Char('i'), KeyModifiers::NONE));

        // Shift+Enter should add newline without submitting
        let result =
            input_manager.handle_key_event(create_key_event(KeyCode::Enter, KeyModifiers::SHIFT));
        assert!(matches!(result, KeyEventResult::Continue));

        // Add more text
        input_manager.handle_key_event(create_key_event(KeyCode::Char('b'), KeyModifiers::NONE));
        input_manager.handle_key_event(create_key_event(KeyCode::Char('y'), KeyModifiers::NONE));
        input_manager.handle_key_event(create_key_event(KeyCode::Char('e'), KeyModifiers::NONE));

        // Should have multiline content
        let content = input_manager.textarea.text();
        assert_eq!(content, "hi\nbye");

        // Regular Enter should submit
        let result =
            input_manager.handle_key_event(create_key_event(KeyCode::Enter, KeyModifiers::NONE));
        match result {
            KeyEventResult::SendMessage { message, .. } => {
                assert_eq!(message, "hi\nbye");
            }
            other => panic!("Expected SendMessage, got {:?}", other),
        }
    }

    #[test]
    fn test_small_paste_inserts_directly() {
        let mut input_manager = InputManager::new();
        input_manager.handle_paste("hello world".to_string());
        assert_eq!(input_manager.textarea.text(), "hello world");
        assert!(input_manager.pending_pastes.is_empty());
    }

    #[test]
    fn test_large_paste_uses_placeholder() {
        let mut input_manager = InputManager::new();
        // Create a paste larger than threshold
        let large_text: String = (0..50).map(|i| format!("line {}\n", i)).collect();
        let line_count = large_text.lines().count();
        input_manager.handle_paste(large_text.clone());

        // Should show placeholder
        let content = input_manager.textarea.text();
        assert!(
            content.contains(&format!("[Pasted {} lines]", line_count)),
            "Expected placeholder in: {}",
            content
        );

        // Pending pastes should have the real content
        assert_eq!(input_manager.pending_pastes.len(), 1);
        assert_eq!(
            input_manager.pending_pastes[0].1,
            large_text.replace("\r\n", "\n").replace('\r', "\n")
        );
    }

    #[test]
    fn test_large_paste_expanded_on_submit() {
        let mut input_manager = InputManager::new();
        input_manager.textarea.insert_str("before ");
        let large_text: String = (0..50).map(|i| format!("line {}\n", i)).collect();
        input_manager.handle_paste(large_text.clone());
        input_manager.textarea.insert_str(" after");

        let content = input_manager.build_submit_content();
        assert!(content.starts_with("before "));
        assert!(content.ends_with(" after"));
        assert!(content.contains("line 0"));
        assert!(content.contains("line 49"));
    }

    #[test]
    fn test_clear_resets_paste_state() {
        let mut input_manager = InputManager::new();
        let large_text: String = (0..50).map(|i| format!("line {}\n", i)).collect();
        input_manager.handle_paste(large_text);
        input_manager.image_counter = 2;
        input_manager.attachments.push(DraftAttachment::Image {
            content: "abc".to_string(),
            mime_type: "image/png".to_string(),
            width: None,
            height: None,
        });

        input_manager.clear();
        assert_eq!(input_manager.textarea.text(), "");
        assert!(input_manager.pending_pastes.is_empty());
        assert!(input_manager.attachments.is_empty());
        assert_eq!(input_manager.image_counter, 0);
    }
}
