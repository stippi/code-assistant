# UI Migration Plan: GPUI to GPUI-Component

This document outlines a structured plan to migrate the Code Assistant UI from direct GPUI primitives to use the enhanced GPUI-Component library, which offers pre-built components with better styling, layout options, and functionality.

## Current Architecture

The current UI is implemented in `crates/code_assistant/src/ui/gpui/` with the following key components:

- **Gpui** (`mod.rs`): Main UI struct that implements the `UserInterface` trait and manages the message queue
- **MessageView** (`message.rs`): Combines the input area and message display
- **TextInput** (`input.rs`): Custom input component with basic text editing functionality
- **MemoryView** (`memory_view.rs`): Sidebar showing working memory, resources, and file trees
- **Elements** (`elements.rs`): Message containers and different types of content blocks

The UI update mechanism uses a polling approach with shared `Arc<Mutex<>>` objects, where the agent thread updates state and sets a flag that triggers UI refreshes in the GPUI main thread.

## Target Architecture

We'll migrate to a more component-based architecture using GPUI-Component, focusing on:

1. Using `Root` as the base component for better styling and container management
2. Replacing custom TextInput with GPUI-Component's more advanced MultiLine TextInput
3. Using Markdown component for rendering text blocks and thinking blocks
4. Using Drawer component for the MemoryView (right sidebar)
5. Maintaining the current state management approach with shared Arc<Mutex<>> objects

## Detailed Migration Steps

### 0. Switch Project Dependencies

In the `code_assistant` crate, replace the `gpui` dependency with the following:

```toml
gpui = { git = "https://github.com/huacnlee/zed.git", branch = "webview" }
gpui-component = { git = "https://github.com/longbridge/gpui-component.git" }
```

### 1. Setup Foundation with Root Component

Create a new implementation that wraps our main view in GPUI-Component's `Root` structure:

```rust
// In src/ui/gpui/mod.rs

pub fn run_app(&self) {
    // ...existing setup...

    app.run(move |cx| {
        // Setup and initialization...

        // Create memory view with our shared working memory
        let memory_view = cx.new(|cx| MemoryView::new(working_memory.clone(), cx));

        // Create window with Root component
        let bounds = gpui::Bounds::centered(None, gpui::size(gpui::px(1000.0), gpui::px(650.0)), cx);
        let window_result = cx.open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some(gpui::SharedString::from("Code Assistant")),
                    appears_transparent: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                // Create TextInput
                let text_input = cx.new(|cx| gpui_component::input::TextInput::new(window, cx)
                    .multi_line()
                    .rows(1)
                    .max_rows(5)
                    .placeholder("Type your message..."));

                // Create MessageView with our TextInput
                let message_view = cx.new(|cx| {
                    MessageView::new(
                        text_input,
                        memory_view.clone(),
                        cx,
                        input_value.clone(),
                        message_queue.clone(),
                        input_requested.clone(),
                    )
                });

                // Wrap everything in a Root component
                cx.new(|cx| gpui_component::Root::new(message_view.into(), window, cx))
            },
        );

        // ...existing focus and refresh setup...
    });
}
```

### 2. Replace TextInput Component

Replace the custom TextInput implementation with GPUI-Component's TextInput:

1. Update the MessageView to use the new TextInput:

```rust
// In src/ui/gpui/message.rs

use gpui_component::input::TextInput;

pub struct MessageView {
    pub text_input: Entity<TextInput>,
    // ...other fields...
}
```

2. Modify the input-related methods to work with the new TextInput API:

```rust
fn on_submit_click(&mut self, _: &MouseUpEvent, window: &mut gpui::Window, cx: &mut Context<Self>) {
    self.text_input.update(cx, |text_input, cx| {
        let content = text_input.value().to_string();
        if !content.is_empty() {
            // Store input in the shared value
            let mut input_value = self.input_value.lock().unwrap();
            *input_value = Some(content);

            // Clear the input field
            text_input.set_value("".into(), window, cx);
        }
    });
    cx.notify();
}
```

### 3. Implement Markdown Rendering for Text Blocks

Use GPUI-Component's Markdown component to render each individual block:

1. Add imports for the Markdown component:

```rust
// In src/ui/gpui/elements.rs

use gpui_component::text::markdown::Markdown;
```

2. Update the text block rendering to use Markdown:

```rust
// In the MessageContainer implementation

pub fn add_text_block(&self, text: String) {
    let mut elements = self.elements.lock().unwrap();
    elements.push(MessageElement::TextBlock(text));
}

// During rendering in message.rs
match element {
    MessageElement::TextBlock(text) => {
        // Use Markdown component for rendering text
        div()
            .child(Markdown::new(ElementId::new("md-block"), text.clone()))
            .into_any_element()
    },
    // ...other element types...
}
```

3. Same approach for thinking blocks:

```rust
// During rendering in message.rs
match element {
    MessageElement::ThinkingBlock(block) => {
        // Create Markdown component for the thinking block content
        let content_element = if block.is_collapsed {
            // If collapsed, show just the first line
            let first_line = block.content.lines().next().unwrap_or("").to_string();
            Markdown::new(ElementId::new("thinking-summary"), first_line + "...")
        } else {
            // If expanded, show the full content
            Markdown::new(ElementId::new("thinking-content"), block.content.clone())
        };

        // Wrap in a styled container with toggle functionality
        div()
            .bg(rgb(0x303040))
            .p_2()
            .rounded_sm()
            .border_l_4()
            .border_color(rgb(0x6060A0))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                // Header with icon and label
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    // ...existing header elements...
            )
            .child(content_element)
            .into_any_element()
    },
    // ...other element types...
}
```

### 4. Convert MemoryView to a Drawer Component

Replace the custom sidebar implementation with GPUI-Component's Drawer:

1. Update imports:

```rust
// In src/ui/gpui/mod.rs

use gpui_component::{ContextModal, Drawer};
```

2. Modify the MessageView to use Drawer for MemoryView:

```rust
// In src/ui/gpui/message.rs

impl MessageView {
    pub fn render_with_memory(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Main content layout
        div()
            .size_full()
            .flex()
            .flex_col()
            // ...existing message and input rendering...
    }

    pub fn toggle_memory_drawer(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if window.has_active_drawer(cx) {
            window.close_drawer(cx);
        } else {
            window.open_drawer(cx, |drawer, window, cx| {
                drawer
                    .size(px(300.))
                    .content(|window, cx| {
                        // Create the MemoryView inside the drawer
                        self.memory_view.clone()
                    })
            });
        }
    }
}
```

3. Simplify the MemoryView structure since it will be contained in a Drawer:

```rust
// In src/ui/gpui/memory_view.rs

impl Render for MemoryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // We don't need the toggle logic anymore since the drawer handles that
        let has_memory = self.memory.lock().unwrap().is_some();

        div()
            .id("memory-sidebar")
            .track_focus(&self.focus_handle(cx))
            .size_full()
            .flex()
            .flex_col()
            .when(has_memory, |this| {
                let memory = self.memory.lock().unwrap().clone().unwrap();
                this
                    .child(self.generate_resource_section(&memory, cx))
                    .child(self.generate_file_tree_section(&memory, cx))
            })
            .when(!has_memory, |this| {
                this.child(
                    div()
                        .p_4()
                        .text_center()
                        .text_color(hsla(0., 0., 0.5, 1.0))
                        .child("No memory data available")
                )
            })
    }
}
```

### 5. Optional: Experiment with Entity-Based Event System (Separate Phase)

As an optional, clearly separated phase, we can experiment with enhancing the current polling-based mechanism with entity events to improve UI responsiveness. This would be implemented only after the other migration steps are complete and stable.

**Note:** This phase is optional and can be skipped if it introduces too much complexity. It is kept separate from the other migration steps to avoid complicating the core migration.

#### Event System Implementation

1. Add event types:

```rust
// In src/ui/gpui/mod.rs

#[derive(Clone, Debug)]
pub enum UiEvent {
    MessageAdded,
    MessageUpdated,
    InputRequested(bool),
    MemoryUpdated,
}
```

2. Create an event dispatcher that works with or without context access:

```rust
// In src/ui/gpui/mod.rs

pub struct UiEventDispatcher {
    // Keep the original update flag for polling
    ui_update_needed: Arc<Mutex<bool>>,
    // Store a weak reference to window for emitting events when possible
    window_ref: Arc<Mutex<Option<WeakWindowHandle<MessageView>>>>,
}

impl UiEventDispatcher {
    pub fn new(ui_update_needed: Arc<Mutex<bool>>) -> Self {
        Self {
            ui_update_needed,
            window_ref: Arc::new(Mutex::new(None)),
        }
    }

    // Called from GPUI thread to register window
    pub fn register_window(&self, window: &WindowHandle<MessageView>) {
        if let Ok(mut window_ref) = self.window_ref.lock() {
            *window_ref = Some(window.downgrade());
        }
    }

    // Can be called from any thread, including agent thread
    pub fn emit_event(&self, event: UiEvent) {
        // Always update the polling flag
        if let Ok(mut flag) = self.ui_update_needed.lock() {
            *flag = true;
        }

        // Try to emit an entity event if we have access to a window
        if let Ok(window_ref) = self.window_ref.lock() {
            if let Some(weak_handle) = window_ref.as_ref() {
                if let Some(window) = weak_handle.upgrade() {
                    // We're lucky - we can emit an entity event directly
                    let event_clone = event.clone();
                    window.update(move |view, _, cx| {
                        view.handle_event(&event_clone, cx);
                    }).ok();
                }
            }
        }
    }
}
```

3. This approach allows us to maintain the polling mechanism while opportunistically using entity events when possible, without requiring `cx` access in the agent thread.

**Implementation note:** This experiment should be conducted separately and should not affect the other migration steps. The current polling mechanism must remain fully functional even if this experiment is attempted.

## UI Component Improvements

### Use Tabs and TabBar for Tool Output

For tool output sections, we can use TabBar to provide better organization:

```rust
// In the MessageContainer's tool rendering logic

fn render_tool_block(&self, tool_block: &ToolBlock) -> impl IntoElement {
    let tab_bar = gpui_component::tab::TabBar::new(ElementId::new("tool-tabs"))
        .child(
            gpui_component::tab::Tab::new("Parameters")
                .active(true)
                .content(|| {
                    // Render parameter inputs
                    // ...
                })
        )
        .child(
            gpui_component::tab::Tab::new("Result")
                .active(false)
                .content(|| {
                    // Render tool output
                    // ...
                })
        );

    div()
        .bg(rgb(0x303535))
        .p_2()
        .rounded_md()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            // Tool header
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                // ...existing header...
        )
        .child(tab_bar)
}
```

### Use Accordion for Collapsible Content

For thinking blocks, we can replace our custom collapsible implementation with the Accordion component:

```rust
fn render_thinking_blocks(blocks: &[ThinkingBlock]) -> impl IntoElement {
    let accordion = gpui_component::Accordion::new(ElementId::new("thinking-blocks"))
        .multiple(true);

    // Add each thinking block as an accordion item
    let accordion_with_items = blocks.iter().enumerate().fold(
        accordion,
        |acc, (idx, block)| {
            acc.child(
                gpui_component::AccordionItem::new(format!("thinking-{}", idx))
                    .title(format!("Thinking Block {}", idx + 1))
                    .expanded(!block.is_collapsed)
                    .content(|| {
                        Markdown::new(
                            ElementId::new(format!("thinking-content-{}", idx)),
                            block.content.clone()
                        )
                    })
            )
        }
    );

    accordion_with_items
}
```



## Implementation Strategy

1. **Phase 1: Foundation Setup**
   - Add GPUI-Component dependency
   - Implement Root component integration
   - Set up event system alongside existing polling

2. **Phase 2: Input Component Migration**
   - Replace TextInput with GPUI-Component TextInput
   - Update message submission logic

3. **Phase 3: Markdown Rendering**
   - Implement Markdown rendering for text blocks
   - Implement Markdown rendering for thinking blocks
   - Update styling for these blocks

4. **Phase 4: Drawer Implementation**
   - Implement MemoryView as a Drawer
   - Update toggle functionality
   - Ensure proper state management

5. **Phase 5: Enhanced Components**
   - Add TabBar for tool outputs
   - Add Accordion for thinking blocks
   - Maintain custom rendering for tool parameters to keep flexibility

## Considerations and Challenges

1. **State Management**
   - The current shared state pattern with Arc<Mutex<>> works well and should be maintained
   - The optional entity events experiment would be implemented separately
   - The core functionality must continue to work with the polling mechanism

2. **Markdown Rendering**
   - Using individual Markdown components per block is the right approach
   - This preserves the current block structure
   - All blocks being rendered independently ensures performance won't be affected

3. **Drawer vs Custom Sidebar**
   - The Drawer component provides built-in open/close animations
   - Width control is standardized
   - Toggle button can be integrated into the app header

4. **Component Initialization**
   - GPUI-Component needs proper initialization in the app setup
   - Theme initialization is particularly important for consistent styling

## Code Locations for Reference

1. Current UI implementation (in project `code-assistant`):
   - `crates/code_assistant/src/ui/gpui/mod.rs`: Main UI struct
   - `crates/code_assistant/src/ui/gpui/message.rs`: Message view
   - `crates/code_assistant/src/ui/gpui/input.rs`: Text input
   - `crates/code_assistant/src/ui/gpui/memory_view.rs`: Memory sidebar
   - `crates/code_assistant/src/ui/gpui/elements.rs`: Message elements

2. GPUI-Component reference (in read-only project `gpui-component`):
   - `crates/ui/src/root.rs`: Root component
   - `crates/ui/src/input/input.rs`: TextInput component
   - `crates/ui/src/text/markdown.rs`: Markdown component
   - `crates/ui/src/drawer.rs`: Drawer component
   - `crates/story/src/main.rs`: Example app structure

These files will serve as the primary references for implementing the migration plan.
