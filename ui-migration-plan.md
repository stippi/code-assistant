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
4. Using the Sidebar component for the MemoryView (right sidebar)
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

### 4. Implement MemoryView as a Sidebar Component with TitleBar Control

Replace the current memory view toggle implementation with GPUI-Component's Sidebar component, with toggle controls in the TitleBar. This approach allows for a clean UI where:
1. The sidebar takes up its own space next to the message area (rather than overlaying it)
2. The toggle button is placed in the window's title bar for better visibility and consistency

Wir werden die Sidebar-Implementierung aus dem `gpui-component`-Projekt (`crates/ui/src/sidebar/mod.rs`) verwenden, die ein sauberes, zusammenklappbares Sidebar-Muster bietet.

1. Update der Imports:

```rust
// In src/ui/gpui/mod.rs
use gpui_component::{
    ContextModal,
    sidebar::{Sidebar, SidebarToggleButton, Side},
    h_flex, v_flex
};
```

2. Hinzufügen von Statusvariablen zur MessageView-Struktur:

```rust
// In src/ui/gpui/message.rs
pub struct MessageView {
    // Bestehende Felder...

    // Neue Felder für die Sidebar-Steuerung
    memory_view_visible: bool,
    memory_collapsed: bool,
}

impl MessageView {
    pub fn new(
        // Bestehende Parameter...
    ) -> Self {
        Self {
            // Bestehende Felder-Initialisierungen...

            // Initialisiere die neuen Felder
            memory_view_visible: true,
            memory_collapsed: false,
        }
    }

    // Toggle-Methode für die Sidebar
    fn toggle_memory_collapsed(&mut self, _: &MouseUpEvent, _window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.memory_collapsed = !self.memory_collapsed;
        cx.notify();
    }

    // Toggle-Methode für die Sidebar-Sichtbarkeit
    fn toggle_memory_visibility(&mut self, _: &MouseUpEvent, _window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.memory_view_visible = !self.memory_view_visible;
        cx.notify();
    }
}
```

3. Modifizieren des MessageView-Renderings für die Sidebar:

```rust
// In src/ui/gpui/message.rs

impl Render for MessageView {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Get current messages and check if input is requested
        let messages = { self.message_queue.lock().unwrap().clone() };
        let is_input_requested = *self.input_requested.lock().unwrap();

        // Main container is now a horizontal layout with a sidebar
        div()
            .on_action(|_: &CloseWindow, window, _| {
                window.remove_window();
            })
            .bg(rgb(0x2c2c2c))
            .track_focus(&self.focus_handle(cx))
            .size_full()
            .pt_8() // Leave room for the window title bar
            .flex()
            .flex_row() // Main container as row layout
            .child(
                // Left side with messages and input (content area)
                div()
                    .flex()
                    .flex_col()
                    .flex_grow() // Grow to take available space
                    .flex_shrink() // Allow shrinking if needed
                    .overflow_hidden() // Prevent overflow
                    .child(
                        // Messages display area with scrollbar
                        div()
                            .id("messages-container")
                            .flex_1() // Take remaining space
                            .relative() // For absolute positioning of scrollbar
                            .child(
                                div()
                                    .id("messages")
                                    .size_full() // Fill parent
                                    .p_2()
                                    .scrollable(window.current_view(), ScrollbarAxis::Vertical)
                                    .bg(rgb(0x202020))
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .text_size(px(18.))
                                    .children(messages.into_iter().map(|msg| {
                                        // Message rendering (unchanged)
                                        // ...
                                    })),
                            )
                    )
                    .child(
                        // Input area (unchanged)
                        // ...
                    ),
            )
            .when(self.memory_view_visible, |this| {
                this.child(
                    // Right sidebar with MemoryView
                    Sidebar::right()
                        .width(px(300.0))
                        .collapsible(true)
                        .collapsed(self.memory_collapsed)
                        .header(
                            // Header with title and collapse button
                            h_flex()
                                .justify_between()
                                .items_center()
                                .child("Working Memory")
                                .child(
                                    SidebarToggleButton::right()
                                        .collapsed(self.memory_collapsed)
                                        .on_click(cx.listener(Self::toggle_memory_collapsed))
                                )
                        )
                        .child(self.memory_view.clone())
                )
            })
    }
}
```

4. Update der MemoryView, um Collapsible zu implementieren:

```rust
// In src/ui/gpui/memory_view.rs

// Import the Collapsible trait
use gpui_component::Collapsible;

impl MemoryView {
    // Bestehende Methoden...

    // Füge eine Methode hinzu, um auf den Collapsed-Status zu reagieren
    fn render_collapsed_state(&mut self, window: &mut Window, cx: &mut Context<Self>, is_collapsed: bool) -> impl IntoElement {
        // Wenn zusammengeklappt, zeige nur Symbole an
        if is_collapsed {
            v_flex()
                .items_center()
                .p_2()
                .gap_4()
                .child(file_icons::render_icon(
                    &file_icons::get().get_type_icon(file_icons::WORKING_MEMORY),
                    24.0,
                    rgb(0xAAAAAA),
                    "🧠"
                ))
        } else {
            // Normaler Inhalt, wenn ausgeklappt
            self.render_memory_content(window, cx)
        }
    }

    // Methode für den normalen Inhalt
    fn render_memory_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_memory = self.memory.lock().unwrap().is_some();

        div()
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

### 5. TitleBar Integration

Um die Sidebar über die Titelleiste zu steuern, musst Du eine neue TitleBar-Komponente implementieren. Im Folgenden wird beschrieben, wie diese Integration funktioniert:

```rust
// Neue Datei: crates/code_assistant/src/ui/gpui/title_bar.rs

use gpui::{
    div, prelude::*, px, AnyElement, App, Context, Entity, MouseButton, Window,
};
use gpui_component::{
    button::Button,
    IconName,
    TitleBar as GpuiTitleBar,
};
use std::rc::Rc;

pub struct TitleBar {
    title: String,
    memory_collapsed: bool,
    memory_visible: bool,
    on_toggle_memory: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
}

impl TitleBar {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            memory_collapsed: false,
            memory_visible: true,
            on_toggle_memory: None,
        }
    }

    pub fn memory_collapsed(mut self, collapsed: bool) -> Self {
        self.memory_collapsed = collapsed;
        self
    }

    pub fn memory_visible(mut self, visible: bool) -> Self {
        self.memory_visible = visible;
        self
    }

    pub fn on_toggle_memory(
        mut self,
        callback: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle_memory = Some(Rc::new(callback));
        self
    }
}

impl RenderOnce for TitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Verwende die TitleBar-Komponente aus gpui-component
        GpuiTitleBar::new()
            // Linke Seite: Titel
            .child(div().flex().items_center().child(self.title))
            // Rechte Seite: Buttons
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .px_2()
                    .gap_2()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        // Memory Toggle Button
                        Button::new(\"memory-toggle\")
                            .small()
                            .ghost()
                            .icon(if self.memory_collapsed {
                                IconName::PanelRightOpen
                            } else {
                                IconName::PanelRightClose
                            })
                            .when_some(self.on_toggle_memory.clone(), |btn, callback| {
                                btn.on_click(move |_, window, cx| {
                                    callback(window, cx);
                                })
                            })
                    )
            )
    }
}
```

Dann integriere diese TitleBar in die Hauptanwendung:

```rust
// In src/ui/gpui/mod.rs

// Füge den Import hinzu
use crate::ui::gpui::title_bar::TitleBar;

pub fn run_app(&self) {
    // ...

    // Erstelle ein MessageView als Hauptkomponente
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

    // Erstelle ein Window mit einer TitleBar
    let window_result = cx.open_window(
        gpui::WindowOptions {
            // ... (bestehende Optionen)
            // Wichtig: `custom_titlebar` auf true setzen
            custom_titlebar: true,
            // ... (weitere Optionen)
        },
        |window, cx| {
            // Erstelle eine Root-Komponente, die TitleBar und Content kombiniert
            cx.new(|cx| {
                let title_bar = TitleBar::new(\"Code Assistant\")
                    .memory_collapsed(message_view.update(cx, |view, _| view.memory_collapsed).unwrap())
                    .on_toggle_memory(move |window, cx| {
                        message_view.update(window, |view, cx| {
                            view.toggle_memory_collapsed(cx);
                        });
                    });

                gpui_component::Root::new(
                    // Vertikales Layout mit TitleBar oben und Content darunter
                    div()
                        .flex()
                        .flex_col()
                        .size_full()
                        .child(title_bar)
                        .child(message_view.clone())
                        .into_any_element(),
                    window,
                    cx
                )
            })
        },
    );

    // ...
}
```

In der MessageView-Komponente musst Du dann eine Methode zum Umschalten des Sidebar-Status hinzufügen:

```rust
// In src/ui/gpui/message.rs

impl MessageView {
    // ...

    pub fn toggle_memory_collapsed(&mut self, cx: &mut Context<Self>) {
        self.memory_collapsed = !self.memory_collapsed;
        cx.notify();
    }

    // Getter-Methode für den Status
    pub fn memory_collapsed(&self) -> bool {
        self.memory_collapsed
    }
}
```

**Referenzen:**
- Die TitleBar ist an `crates/story/src/title_bar.rs` angelehnt
- Die Implementierung des Toggle-Buttons ist inspiriert von `crates/story/src/sidebar_story.rs`


### 6. Optional: Experiment with Entity-Based Event System (Separate Phase)

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

1. **Phase 1: Foundation Setup** ✅
   - ✅ Add GPUI-Component dependency
   - ✅ Implement Root component integration
   - Set up event system alongside existing polling

2. **Phase 2: Input Component Migration** ✅
   - ✅ Replace TextInput with GPUI-Component TextInput
   - ✅ Update message submission logic

3. **Phase 3: Markdown Rendering** ✅
   - ✅ Implement Markdown rendering for text blocks
   - ✅ Implement Markdown rendering for thinking blocks
   - ✅ Update styling for these blocks

4. **Phase 4: Drawer Implementation** ✅
   - ✅ Implement MemoryView as a Drawer
   - ✅ Update toggle functionality
   - ✅ Ensure proper state management

5. **Phase 5: Enhanced Components**
   - Add TabBar for tool outputs
   - Add Accordion for thinking blocks
   - Maintain custom rendering for tool parameters to keep flexibility

6. **Phase 6: Theme Integration**
   - Migrate from hard-coded colors to gpui-component's theme system
   - Create a consistent dark theme based on current UI colors
   - Update all UI components to use themed colors instead of hard-coded values

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
