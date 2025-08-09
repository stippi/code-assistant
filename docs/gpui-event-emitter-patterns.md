# GPUI EventEmitter and Subscriber Patterns

This document describes EventEmitter and Subscriber patterns in GPUI, based on examples from the Zed and gpui-component projects.

## Basic Concept

In GPUI, any type can be made into an EventEmitter by implementing the `EventEmitter<T>` trait. Events can contain custom data and are managed through the Context system.

## Implementing EventEmitter

### 1. Defining Event Types

```rust
// Simple event without data
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    increment: usize,
}

// Enum for different event types
#[derive(Clone)]
pub enum InputEvent {
    Change(SharedString),
    PressEnter { secondary: bool },
    Focus,
    Blur,
}

// Complex event with multiple variants
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorEvent {
    InputIgnored { text: Arc<str> },
    InputHandled {
        utf16_range_to_replace: Option<Range<isize>>,
        text: Arc<str>,
    },
    Focused,
    Blurred,
    SelectionsChanged { local: bool },
    ScrollPositionChanged { local: bool, autoscroll: bool },
    // ... more variants
}
```

### 2. Implementing the EventEmitter Trait

```rust
use gpui::EventEmitter;

struct Counter {
    count: usize,
}

// Simple implementation - no additional logic required
impl EventEmitter<Change> for Counter {}

// A type can emit multiple events
impl EventEmitter<InputEvent> for InputState {}
impl EventEmitter<DismissEvent> for InputState {}

// Complex types with multiple event types
impl EventEmitter<PanelEvent> for TabPanel {}
impl EventEmitter<DismissEvent> for TabPanel {}
```

## Emitting Events

Events are triggered through the Context using `cx.emit()`:

```rust
impl InputState {
    fn on_focus(&mut self, cx: &mut Context<Self>) {
        // Start cursor
        self.blink_cursor.update(cx, |cursor, cx| {
            cursor.start(cx);
        });

        // Emit event
        cx.emit(InputEvent::Focus);
    }

    fn on_text_change(&mut self, cx: &mut Context<Self>) {
        // Business logic
        self.mode.update_auto_grow(&self.text_wrapper);

        // Emit event with data
        cx.emit(InputEvent::Change(self.unmask_value()));
        cx.notify(); // Mark view for re-render
    }

    fn handle_enter(&mut self, action: &Enter, cx: &mut Context<Self>) {
        // Event with structured data
        cx.emit(InputEvent::PressEnter {
            secondary: action.secondary,
        });
    }
}
```

## Registering Subscribers

### 1. Basic Subscribe Pattern

```rust
fn setup_subscriptions(
    counter: &Entity<Counter>,
    cx: &mut Context<SomeView>
) -> Vec<Subscription> {
    vec![
        // Simple subscriber
        cx.subscribe(counter, |subscriber, _emitter, event, _cx| {
            subscriber.count += event.increment * 2;
        })
    ]
}
```

### 2. Subscriber with Context Access

```rust
fn setup_editor_subscriptions(
    editor: &Entity<Editor>,
    cx: &mut Context<SearchView>
) -> Vec<Subscription> {
    vec![
        cx.subscribe(editor, |this, _editor, event: &EditorEvent, cx| {
            match event {
                EditorEvent::BufferEdited => {
                    // React to changes
                    this.update_search_results(cx);
                }
                EditorEvent::SelectionsChanged { .. } => {
                    this.update_match_index(cx);
                }
                _ => {}
            }
        })
    ]
}
```

### 3. Event Forwarding (Event Bubbling)

```rust
// Forward events to parent view
cx.subscribe(&child_editor, |_, _, event: &EditorEvent, cx| {
    cx.emit(ViewEvent::EditorEvent(event.clone()))
})

// Forward specific events
cx.subscribe(&rename_editor, |_, _, event: &EditorEvent, cx| {
    if event == &EditorEvent::Focused {
        cx.emit(EditorEvent::FocusedIn)
    }
})
```

### 4. Subscriber with Closure and Move Semantics

```rust
fn setup_complex_subscriptions(
    input_state: &Entity<InputState>,
    webview: Entity<WebView>,
    cx: &mut Context<WebViewStory>
) -> Vec<Subscription> {
    vec![
        cx.subscribe(input_state, {
            let webview = webview.clone();
            move |this, input, event: &InputEvent, cx| {
                match event {
                    InputEvent::PressEnter { .. } => {
                        let url = input.read(cx).text();
                        webview.update(cx, |webview, _| {
                            webview.load_url(&url);
                        });
                    }
                    InputEvent::Change(text) => {
                        this.current_url = text.clone();
                        cx.notify();
                    }
                    _ => {}
                }
            }
        })
    ]
}
```

## Context Types and Access

### Understanding Context Parameters

```rust
cx.subscribe(entity, |subscriber, emitter, event, cx| {
    // subscriber: &mut Self - The subscribing type (mutable)
    // emitter: Entity<T> - The entity that emitted the event
    // event: &EventType - The emitted event
    // cx: &mut Context<Self> - Context for the subscriber type
})
```

### Different Context Access Patterns

```rust
// 1. Modifying subscriber state
cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
    this.input_value = match event {
        InputEvent::Change(text) => text.clone(),
        _ => this.input_value.clone(),
    };
    cx.notify(); // Re-render view
})

// 2. Reading emitter state
cx.subscribe(&counter, |this, emitter, event: &Change, cx| {
    let current_count = emitter.read(cx).count;
    this.display_value = current_count + event.increment;
})

// 3. Modifying emitter state
cx.subscribe(&input, |_, emitter, event: &InputEvent, cx| {
    emitter.update(cx, |input_state, cx| {
        input_state.validate();
        cx.emit(InputEvent::ValidationComplete);
    });
})

// 4. Updating other entities
cx.subscribe(&source, |this, _, event, cx| {
    this.other_entity.update(cx, |other, cx| {
        other.handle_external_event(event);
        cx.notify();
    });
})
```

## Subscription Management

### 1. Storing Subscriptions

```rust
pub struct MyView {
    input_state: Entity<InputState>,
    // Subscriptions must be stored to remain active
    _subscriptions: Vec<Subscription>,
}

impl MyView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input_state = cx.new(|cx| InputState::new(window, cx));

        let _subscriptions = vec![
            cx.subscribe(&input_state, |this, _, event: &InputEvent, cx| {
                // Event handling
            }),
            // More subscriptions...
        ];

        Self {
            input_state,
            _subscriptions,
        }
    }
}
```

### 2. Detached Subscriptions

```rust
// Immediately "detach" subscription - managed automatically
cx.subscribe(&entity, |_, _, event, _| {
    println!("Event received: {:?}", event);
}).detach();

// Useful for fire-and-forget event handling
cx.observe(&blink_cursor, |_, _, cx| cx.notify()).detach();
```

### 3. Conditional Subscriptions

```rust
// Subscription based on state
if self.should_listen_to_input {
    subscriptions.push(
        cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            this.handle_input_event(event, cx);
        })
    );
}
```

## Practical Patterns

### 1. State Synchronization Between Components

```rust
// Parent listens to child events and updates another child
cx.subscribe(&slider, |this, _, event: &SliderEvent, cx| {
    match event {
        SliderEvent::Change(value) => {
            this.value_display.update(cx, |display, cx| {
                display.set_value(*value);
                cx.notify();
            });
        }
    }
})
```

### 2. Form-Validation Pattern

```rust
struct FormView {
    inputs: Vec<Entity<InputState>>,
    _subscriptions: Vec<Subscription>,
}

impl FormView {
    fn setup_validation(&mut self, cx: &mut Context<Self>) {
        for input in &self.inputs {
            self._subscriptions.push(
                cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    match event {
                        InputEvent::Change(_) | InputEvent::Blur => {
                            this.validate_form(cx);
                        }
                        _ => {}
                    }
                })
            );
        }
    }
}
```

### 3. Event Aggregation Pattern

```rust
// Aggregate multiple similar events into one
cx.subscribe(&editor, |this, _, event: &EditorEvent, cx| {
    match event {
        EditorEvent::BufferEdited |
        EditorEvent::SelectionsChanged { .. } |
        EditorEvent::ScrollPositionChanged { .. } => {
            this.schedule_update(cx);
        }
        _ => {}
    }
})
```

### 4. Notification System Pattern

```rust
impl NotificationManager {
    fn add_notification(&mut self, notification: Entity<Notification>, cx: &mut Context<Self>) {
        let id = notification.read(cx).id.clone();

        // Auto-dismiss after timeout
        self._subscriptions.insert(
            id.clone(),
            cx.subscribe(&notification, move |this, _, _: &DismissEvent, cx| {
                this.notifications.retain(|n| n.read(cx).id != id);
                this._subscriptions.remove(&id);
                cx.notify();
            })
        );
    }
}
```

## Best Practices

### 1. Event Design
- **Use Enums** for different event types of a component
- **Include relevant data** in events, not just signals
- **Clone-able events** enable easy forwarding
- **Structured data** instead of primitive types for better API

### 2. Subscription Management
- **Store subscriptions** in `_subscriptions: Vec<Subscription>`
- **Use detach()** only for fire-and-forget scenarios
- **Cleanup on component destruction** happens automatically

### 3. Context Usage
- **Minimize context accesses** for better performance
- **Batch updates** with `cx.notify()` at the end
- **Use update()** for safe entity modification

### 4. Performance Considerations
- **Filter events early** to avoid unnecessary processing
- **Debounce frequent events** when necessary
- **Use specific event types** instead of generic ones

## Common Pitfalls

1. **Forgotten Subscription Storage**: Subscriptions must be stored in the struct
2. **Circular Event Loops**: Be careful with event forwarding
3. **Context Type Confusion**: The context always belongs to the subscriber, not the emitter
4. **Missing cx.notify()**: Required after state changes for re-rendering

This pattern enables loosely coupled, reactive UI components with clear data flow architecture.
