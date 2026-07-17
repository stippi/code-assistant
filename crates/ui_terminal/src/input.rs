use std::collections::HashMap;

use base64::Engine;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tracing::debug;

use code_assistant_core::persistence::DraftAttachment;

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

    /// Clear conversation context
    ClearContext,
    /// Compact (summarise) conversation context
    CompactContext,
    /// Open the skill picker popup.
    OpenSkillPicker,
    /// Open the session picker popup.
    OpenSessionPicker,
    /// Switch to another session by id.
    SwitchSession(String),
    /// Activate a skill. `scope` is the scope token, or `None` to resolve it
    /// from the cached catalog by name.
    InvokeSkill { scope: Option<String>, name: String },
    /// Manage the session's durable goals (`/goal`): the raw argument text.
    Goal { args: String },
    /// Show the current permission tier.
    ShowPermissionTier,
    /// Switch the permission tier.
    SetPermissionTier(tools_core::permissions::PermissionTier),
    /// Answer a tool permission request (`None` = oldest pending).
    RespondPermission {
        request_id: Option<String>,
        decision: tools_core::PermissionDecision,
    },
    /// The slash-prefix on the current input line changed.
    ///
    /// `Some("")` means the user just typed `/` (open the popup at root).
    /// `Some("cl")` means the line is `/cl…` (filter root popup by "cl").
    /// `None` means the current line no longer starts with `/` (close the
    /// popup if it was open).
    SlashPrefixChanged(Option<String>),
    /// The user typed (or backspaced) while a popup was already open.
    /// The string is the entire current composer line, used as the query
    /// for the top-of-stack popup (no leading slash strip).
    PopupQueryChanged(String),
    /// A key event that should be routed to the active popup (Up / Down /
    /// Enter / Tab / Esc while the popup is open).
    PopupKey(KeyEvent),
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
    /// Whether a slash-command popup is currently visible.
    ///
    /// When `true`, Up / Down / Enter / Tab / Esc are intercepted and routed
    /// to the popup stack as [`KeyEventResult::PopupKey`] events. The flag is
    /// set/cleared by the app event loop based on the popup-stack depth.
    pub popup_active: bool,
}

impl InputManager {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let command_processor = CommandProcessor::new().ok();
        Self {
            textarea: TextArea::new(),
            command_processor,
            attachments: Vec::new(),
            image_counter: 0,
            pending_pastes: Vec::new(),
            large_paste_counters: HashMap::new(),
            popup_active: false,
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
            // Escape: close popup if open, otherwise bubble up.
            KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if self.popup_active {
                    KeyEventResult::PopupKey(key_event)
                } else {
                    KeyEventResult::Escape
                }
            }
            // Up/Down: navigate the popup when it is open.
            KeyEvent {
                code: KeyCode::Up,
                modifiers: KeyModifiers::NONE,
                ..
            } if self.popup_active => KeyEventResult::PopupKey(key_event),
            KeyEvent {
                code: KeyCode::Down,
                modifiers: KeyModifiers::NONE,
                ..
            } if self.popup_active => KeyEventResult::PopupKey(key_event),
            // Tab: route to popup (e.g. accept root suggestion).
            KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::NONE,
                ..
            } if self.popup_active => KeyEventResult::PopupKey(key_event),
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::SHIFT,
                ..
            } => {
                self.textarea.insert_str("\n");
                // A newline on the current line means no slash-command on this line anymore.
                KeyEventResult::SlashPrefixChanged(self.slash_prefix())
            }
            // Plain Enter while popup is open: route to popup so the highlighted
            // row can be activated. The popup stack decides whether this commits
            // a command, pushes a sub-popup, or is a no-op.
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } if self.popup_active => KeyEventResult::PopupKey(key_event),
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
                            CommandResult::ClearContext => KeyEventResult::ClearContext,
                            CommandResult::CompactContext => KeyEventResult::CompactContext,
                            CommandResult::OpenSkillPicker => KeyEventResult::OpenSkillPicker,
                            CommandResult::OpenSessionPicker => KeyEventResult::OpenSessionPicker,
                            CommandResult::SwitchSession(id) => KeyEventResult::SwitchSession(id),
                            CommandResult::InvokeSkill { scope, name } => {
                                KeyEventResult::InvokeSkill { scope, name }
                            }
                            CommandResult::Goal { args } => KeyEventResult::Goal { args },
                            CommandResult::InsertInputTemplate(template) => {
                                self.textarea.insert_str(&template);
                                KeyEventResult::Continue
                            }
                            CommandResult::ShowPermissionTier => KeyEventResult::ShowPermissionTier,
                            CommandResult::SetPermissionTier(tier) => {
                                KeyEventResult::SetPermissionTier(tier)
                            }
                            CommandResult::RespondPermission {
                                request_id,
                                decision,
                            } => KeyEventResult::RespondPermission {
                                request_id,
                                decision,
                            },
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
                // Forward the key event directly to our custom TextArea.
                self.textarea.input(key_event);
                if self.popup_active {
                    // While a popup is open, the entire composer line acts as
                    // the popup query (the leading slash, if any, is stripped
                    // for root popups by the app event loop).
                    KeyEventResult::PopupQueryChanged(self.textarea.text().to_string())
                } else {
                    // No popup open: detect the start of a slash command.
                    KeyEventResult::SlashPrefixChanged(self.slash_prefix())
                }
            }
        }
    }

    /// Returns `Some(query)` when the current input line starts with `/`, where
    /// `query` is the text after the `/`.  Returns `None` otherwise.
    ///
    /// Used after every keystroke to decide whether to show/update/hide the
    /// autocomplete popup.
    pub fn slash_prefix(&self) -> Option<String> {
        let line = self.textarea.current_line();
        line.strip_prefix('/').map(|s| s.to_string())
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

        // Test character input: normal text returns SlashPrefixChanged(None).
        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('h'), KeyModifiers::NONE));
        assert!(matches!(result, KeyEventResult::SlashPrefixChanged(None)));

        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('i'), KeyModifiers::NONE));
        assert!(matches!(result, KeyEventResult::SlashPrefixChanged(None)));

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

        // Shift+Enter should add newline without submitting; returns SlashPrefixChanged.
        let result =
            input_manager.handle_key_event(create_key_event(KeyCode::Enter, KeyModifiers::SHIFT));
        assert!(matches!(result, KeyEventResult::SlashPrefixChanged(_)));

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

    #[test]
    fn test_slash_prefix_detected() {
        let mut input_manager = InputManager::new();

        // Typing '/' should emit SlashPrefixChanged with an empty query string.
        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(
            matches!(result, KeyEventResult::SlashPrefixChanged(Some(ref q)) if q.is_empty()),
            "Expected SlashPrefixChanged with empty query, got {result:?}"
        );

        // Typing 'm' after '/' should emit query "m".
        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('m'), KeyModifiers::NONE));
        assert!(
            matches!(result, KeyEventResult::SlashPrefixChanged(Some(ref q)) if q == "m"),
            "Expected query 'm', got {result:?}"
        );
    }

    #[test]
    fn test_slash_prefix_absent_for_normal_text() {
        let mut input_manager = InputManager::new();
        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('h'), KeyModifiers::NONE));
        assert!(
            matches!(result, KeyEventResult::SlashPrefixChanged(None)),
            "Expected no slash prefix for normal text, got {result:?}"
        );
    }

    #[test]
    fn test_escape_routes_to_popup_when_active() {
        let mut input_manager = InputManager::new();
        input_manager.popup_active = true;

        let result =
            input_manager.handle_key_event(create_key_event(KeyCode::Esc, KeyModifiers::NONE));
        // When popup is open, Escape becomes a PopupKey so the stack can decide
        // whether to pop one level or close entirely.
        assert!(
            matches!(
                result,
                KeyEventResult::PopupKey(KeyEvent {
                    code: KeyCode::Esc,
                    ..
                })
            ),
            "Expected PopupKey(Esc) when popup is open, got {result:?}"
        );
    }

    #[test]
    fn test_escape_propagates_when_popup_inactive() {
        let mut input_manager = InputManager::new();
        let result =
            input_manager.handle_key_event(create_key_event(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            matches!(result, KeyEventResult::Escape),
            "Expected Escape when popup is closed, got {result:?}"
        );
    }

    #[test]
    fn test_arrow_keys_route_to_popup_when_active() {
        let mut input_manager = InputManager::new();
        input_manager.popup_active = true;

        let down =
            input_manager.handle_key_event(create_key_event(KeyCode::Down, KeyModifiers::NONE));
        assert!(
            matches!(
                down,
                KeyEventResult::PopupKey(KeyEvent {
                    code: KeyCode::Down,
                    ..
                })
            ),
            "Expected PopupKey(Down), got {down:?}"
        );

        let up = input_manager.handle_key_event(create_key_event(KeyCode::Up, KeyModifiers::NONE));
        assert!(
            matches!(
                up,
                KeyEventResult::PopupKey(KeyEvent {
                    code: KeyCode::Up,
                    ..
                })
            ),
            "Expected PopupKey(Up), got {up:?}"
        );
    }

    #[test]
    fn test_tab_routes_to_popup_when_active() {
        let mut input_manager = InputManager::new();
        input_manager.popup_active = true;

        let result =
            input_manager.handle_key_event(create_key_event(KeyCode::Tab, KeyModifiers::NONE));
        assert!(
            matches!(
                result,
                KeyEventResult::PopupKey(KeyEvent {
                    code: KeyCode::Tab,
                    ..
                })
            ),
            "Expected PopupKey(Tab), got {result:?}"
        );
    }

    #[test]
    fn test_enter_routes_to_popup_when_active() {
        let mut input_manager = InputManager::new();
        input_manager.popup_active = true;

        let result =
            input_manager.handle_key_event(create_key_event(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(
                result,
                KeyEventResult::PopupKey(KeyEvent {
                    code: KeyCode::Enter,
                    ..
                })
            ),
            "Expected PopupKey(Enter) while popup active, got {result:?}"
        );
    }

    #[test]
    fn test_typing_while_popup_active_emits_popup_query_changed() {
        let mut input_manager = InputManager::new();
        input_manager.popup_active = true;

        // Composer is empty (e.g. just after a sub-popup was pushed).
        // Typing 'c' should NOT close the popup — it should report the
        // composer's current text as a query update.
        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(
            matches!(result, KeyEventResult::PopupQueryChanged(ref q) if q == "c"),
            "Expected PopupQueryChanged(\"c\") while popup active, got {result:?}"
        );

        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('l'), KeyModifiers::NONE));
        assert!(
            matches!(result, KeyEventResult::PopupQueryChanged(ref q) if q == "cl"),
            "Expected PopupQueryChanged(\"cl\"), got {result:?}"
        );
    }

    #[test]
    fn test_backspace_while_popup_active_emits_popup_query_changed() {
        let mut input_manager = InputManager::new();
        input_manager.textarea.insert_str("cl");
        input_manager.popup_active = true;

        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(
            matches!(result, KeyEventResult::PopupQueryChanged(ref q) if q == "c"),
            "Expected PopupQueryChanged(\"c\") after Backspace, got {result:?}"
        );
    }

    #[test]
    fn test_typing_while_popup_inactive_still_emits_slash_prefix_changed() {
        // Regression: popup_active=false must keep the existing slash-prefix
        // detection behaviour (used by the root popup trigger via "/").
        let mut input_manager = InputManager::new();
        let result = input_manager
            .handle_key_event(create_key_event(KeyCode::Char('h'), KeyModifiers::NONE));
        assert!(
            matches!(result, KeyEventResult::SlashPrefixChanged(None)),
            "Expected SlashPrefixChanged(None) when popup inactive, got {result:?}"
        );
    }
}
