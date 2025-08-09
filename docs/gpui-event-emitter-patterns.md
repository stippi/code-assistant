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
        // Simple subscriber with closure
        cx.subscribe(counter, |subscriber, _emitter, event, _cx| {
            subscriber.count += event.increment * 2;
        })
    ]
}
```

### 2. Method Reference Pattern

When the subscriber type has a method with the correct signature, you can often use method references for cleaner code:

```rust
impl SomeView {
    // Method with the right signature for subscription
    fn handle_counter_change(&mut self, _emitter: Entity<Counter>, event: &Change, _cx: &mut Context<Self>) {
        self.count += event.increment * 2;
    }

    // Alternative method names for different event handling
    fn on_input_event(&mut self, _emitter: Entity<InputState>, event: &InputEvent, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change(text) => {
                self.current_text = text.clone();
                cx.notify();
            }
            InputEvent::Focus => self.is_focused = true,
            _ => {}
        }
    }
}

fn setup_subscriptions(
    counter: &Entity<Counter>,
    input: &Entity<InputState>,
    cx: &mut Context<SomeView>
) -> Vec<Subscription> {
    vec![
        // Elegant method reference - no closure needed!
        cx.subscribe(counter, Self::handle_counter_change),
        cx.subscribe(input, Self::on_input_event),

        // Still use closures when you need to capture external variables
        cx.subscribe(counter, {
            let external_multiplier = 5;
            move |subscriber, _emitter, event, _cx| {
                subscriber.count += event.increment * external_multiplier;
            }
        })
    ]
}
```

### Real-World Example from gpui-component

```rust
// From InputStory - clean method reference usage
struct InputStory {
    input1: Entity<InputState>,
    input2: Entity<InputState>,
    phone_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl InputStory {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input1 = cx.new(|cx| InputState::new(window, cx));
        let input2 = cx.new(|cx| InputState::new(window, cx));
        let phone_input = cx.new(|cx| InputState::new(window, cx));

        let _subscriptions = vec![
            // Clean method references - no closures needed!
            cx.subscribe_in(&input1, window, Self::on_input_event),
            cx.subscribe_in(&input2, window, Self::on_input_event),
            cx.subscribe_in(&phone_input, window, Self::on_input_event),
        ];

        Self {
            input1,
            input2,
            phone_input,
            _subscriptions,
        }
    }

    // Method with the exact signature expected by cx.subscribe_in
    fn on_input_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change(text) => println!("Change: {}", text),
            InputEvent::PressEnter { secondary } => println!("PressEnter secondary: {}", secondary),
            InputEvent::Focus => println!("Focus"),
            InputEvent::Blur => println!("Blur"),
        }
    }
}
```

### 3. Subscriber with Context Access

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

### 4. Event Forwarding (Event Bubbling)

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

### 5. Subscriber with Closure and Move Semantics

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
- **Prefer method references** over closures when no external capture is needed: `cx.subscribe(entity, Self::method_name)`

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

## GPUI Entities and Views

In GPUI, any type can be wrapped into an `Entity` using `cx.new()`, where `cx` is an instance of `gpui::App`. Entities are handles that can be freely cloned and passed around, providing a safe way to reference and manipulate data.

### Creating Entities

```rust
use gpui::{App, Context, Entity, Render, Window};

// Any struct can become an Entity
struct Counter {
    count: usize,
}

impl Counter {
    fn new() -> Self {
        Self { count: 0 }
    }

    fn increment(&mut self) {
        self.count += 1;
    }
}

// Create an entity using cx.new()
fn create_counter(cx: &mut App) -> Entity<Counter> {
    cx.new(|_cx| Counter::new())
}

// Entities can be cloned freely
let counter1 = create_counter(cx);
let counter2 = counter1.clone(); // Same entity, different handle
```

### Views: Entities with Render Trait

When a type implements the `Render` trait, it becomes a "View" - an entity that can be rendered in the UI.

```rust
impl Render for Counter {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .child(format!("Count: {}", self.count))
            .child(
                button("Increment")
                    .on_click(cx.listener(|this, _event, cx| {
                        this.increment();
                        cx.notify(); // Trigger UI update
                    }))
            )
    }
}
```

### RenderOnce Trait

For simple, stateless components that don't need to be entities, you can implement `RenderOnce`:

```rust
struct SimpleIcon {
    name: String,
}

impl RenderOnce for SimpleIcon {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div().child(format!("Icon: {}", self.name))
    }
}

// Usage in render method
div().child(SimpleIcon { name: "star".to_string() })
```

### Using View Entities as Children

View entities can be directly used as children in `div().child()` or `.children()`:

```rust
struct ParentView {
    input: Entity<InputState>,
    counter: Entity<Counter>,
}

impl Render for ParentView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .child(self.input.clone())  // Entity as child
            .child(self.counter.clone()) // Another entity as child
            .children([
                self.input.clone(),      // Can also use in children()
                self.counter.clone(),
            ])
    }
}
```

### Updating Entities from Subscriptions

A crucial pattern in EventEmitter systems is updating entities from within subscriptions using `entity.update(cx, closure)`:

```rust
struct FormView {
    input_state: Entity<InputState>,
    webview: Entity<WebView>,
    current_url: String,
    _subscriptions: Vec<Subscription>,
}

impl FormView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Enter URL...")
        });

        let webview = cx.new(|cx| WebView::new(window, cx));

        let _subscriptions = vec![
            // Subscribe to input events and update webview
            cx.subscribe(&input_state, {
                let webview = webview.clone();
                move |this, input, event: &InputEvent, cx| {
                    match event {
                        InputEvent::PressEnter { .. } => {
                            // Read input value
                            let url = input.read(cx).value().clone();

                            // Update webview entity
                            webview.update(cx, |webview, _inner_cx| {
                                webview.load_url(&url);
                            });

                            // Update subscriber state
                            this.current_url = url;
                            cx.notify(); // Trigger re-render of this view
                        }
                        InputEvent::Change(text) => {
                            this.current_url = text.clone();
                            cx.notify();
                        }
                        _ => {}
                    }
                }
            })
        ];

        Self {
            input_state,
            webview,
            current_url: String::new(),
            _subscriptions,
        }
    }
}
```

### Entity Update Pattern Details

The `entity.update(cx, closure)` pattern provides safe access to the entity's inner data:

```rust
// The closure signature is: |inner_data: &mut T, inner_cx: &mut Context<T>|
entity.update(cx, |inner_data, inner_cx| {
    // inner_data: &mut ActualType - mutable reference to the wrapped data
    // inner_cx: &mut Context<ActualType> - context for the inner entity

    // Modify the inner data
    inner_data.some_method();

    // Trigger re-render of the entity if it's a View
    inner_cx.notify();

    // Can emit events from within the entity
    inner_cx.emit(SomeEvent::StateChanged);
});
```

### Complete Example: Input with Validation

```rust
struct ValidatedInput {
    input: Entity<InputState>,
    validator: Entity<Validator>,
    is_valid: bool,
    _subscriptions: Vec<Subscription>,
}

struct Validator {
    pattern: regex::Regex,
    last_result: bool,
}

impl EventEmitter<ValidationEvent> for Validator {}

#[derive(Clone)]
enum ValidationEvent {
    ValidationChanged { is_valid: bool },
}

impl ValidatedInput {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Enter email...")
        });

        let validator = cx.new(|_cx| Validator {
            pattern: regex::Regex::new(r"^[^@]+@[^@]+\.[^@]+$").unwrap(),
            last_result: false,
        });

        let _subscriptions = vec![
            // Input changes trigger validation
            cx.subscribe(&input, {
                let validator = validator.clone();
                move |this, input, event: &InputEvent, cx| {
                    match event {
                        InputEvent::Change(text) => {
                            // Update validator entity
                            validator.update(cx, |validator, inner_cx| {
                                let is_valid = validator.pattern.is_match(text);
                                let changed = validator.last_result != is_valid;
                                validator.last_result = is_valid;

                                if changed {
                                    inner_cx.emit(ValidationEvent::ValidationChanged { is_valid });
                                }
                            });
                        }
                        _ => {}
                    }
                }
            }),

            // Validation results update UI state
            cx.subscribe(&validator, |this, _validator, event: &ValidationEvent, cx| {
                match event {
                    ValidationEvent::ValidationChanged { is_valid } => {
                        this.is_valid = *is_valid;
                        cx.notify(); // Re-render to show validation state
                    }
                }
            })
        ];

        Self {
            input,
            validator,
            is_valid: false,
            _subscriptions,
        }
    }
}

impl Render for ValidatedInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border_color = if self.is_valid {
            cx.theme().success
        } else {
            cx.theme().danger
        };

        div()
            .border_1()
            .border_color(border_color)
            .child(self.input.clone())
            .child(
                div().child(if self.is_valid {
                    "✓ Valid email"
                } else {
                    "✗ Invalid email format"
                })
            )
    }
}
```

### Key Entity Patterns

1. **Entity Creation**: Use `cx.new(|cx| YourType::new())` to create entities
2. **Entity Cloning**: Entities can be cloned freely - they're handles, not the data itself
3. **View Entities**: Implement `Render` trait to make entities renderable
4. **Direct Child Usage**: View entities can be used directly in `.child()` and `.children()`
5. **Safe Updates**: Use `entity.update(cx, |data, inner_cx| { ... })` for safe mutation
6. **Notification**: Call `cx.notify()` after state changes to trigger re-renders
7. **Event Emission**: Use `inner_cx.emit(event)` from within update closures

This entity system provides a robust foundation for building reactive UI components that can safely share state and communicate through events while maintaining clear ownership and lifecycle management.
