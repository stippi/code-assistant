# GPUI Testing Guide

Reference for writing UI tests using the `#[gpui::test]` framework, based on patterns from the Zed project.

## Test Setup

Tests use the `#[gpui::test]` proc macro which automatically creates a `TestAppContext`:

```rust
use gpui::{TestAppContext, VisualTestContext};

#[gpui::test]
fn test_sync_example(cx: &mut TestAppContext) {
    // synchronous test
}

#[gpui::test]
async fn test_async_example(cx: &mut TestAppContext) {
    // async test - can await tasks and conditions
}
```

Multiple test contexts can be requested for multi-window or multi-client scenarios:

```rust
#[gpui::test]
async fn test_multi(cx_a: &TestAppContext, cx_b: &TestAppContext) {
    // two independent app contexts
}
```

## Creating Windows and Views

### Pattern A: `cx.open_window()` — returns `WindowHandle<V>`

```rust
let window = cx.update(|cx| {
    cx.open_window(Default::default(), |_, cx| {
        cx.new(|cx| MyView {
            focus_handle: cx.focus_handle(),
            value: 0,
        })
    }).unwrap()
});
```

### Pattern B: `cx.add_empty_window()` — returns `&mut VisualTestContext`

Useful for testing elements in isolation without a real view:

```rust
let cx = cx.add_empty_window();
cx.draw(point(px(0.), px(0.)), size(px(100.), px(20.)), |_, cx| {
    cx.new(|_| TestView(state.clone()))
});
```

### Pattern C: `cx.add_window_view()` — returns `(Entity<V>, &mut VisualTestContext)`

Convenience that gives both the entity handle and a visual context:

```rust
let (view, cx) = cx.add_window_view(|window, cx| MyComponent::new(window, cx));
```

## Reading and Updating Entity State

```rust
// Read state (immutable)
window.update(cx, |view, _window, _cx| {
    assert_eq!(view.counter, 5);
}).unwrap();

// Or with a separate entity handle
entity.read_with(cx, |state, _cx| {
    assert!(state.is_ready);
});

// Mutate state
entity.update(cx, |state, cx| {
    state.increment();
    cx.notify(); // trigger re-render
});

// Mutate with window access
entity.update_in(cx, |state, window, cx| {
    window.focus(&state.focus_handle);
    state.do_something(window, cx);
});
```

## Simulating User Input

### Keystrokes

```rust
// Single keystroke
cx.dispatch_keystroke(*window, Keystroke::parse("enter").unwrap());

// Multiple keystrokes (space-separated)
cx.simulate_keystrokes("cmd-shift-p escape");

// Text input
cx.simulate_input("hello world");
```

### Mouse Events

```rust
use gpui::{point, px, Modifiers, MouseButton};

// Click at position
cx.simulate_click(point(px(50.), px(25.)), Modifiers::default());

// Granular mouse control
cx.simulate_mouse_down(point(px(50.), px(25.)), MouseButton::Left, Modifiers::default());
cx.simulate_mouse_move(point(px(100.), px(25.)), None, Modifiers::default());
cx.simulate_mouse_up(point(px(100.), px(25.)), MouseButton::Left, Modifiers::default());

// Scroll
cx.simulate_event(ScrollWheelEvent {
    position: point(px(50.), px(50.)),
    delta: ScrollDelta::Pixels(point(px(0.), px(-200.))),
    ..Default::default()
});
```

### Actions

```rust
use gpui::{actions, KeyBinding};

actions!(my_module, [DoSomething, Cancel]);

// Bind keys in test
cx.update(|cx| {
    cx.bind_keys(vec![
        KeyBinding::new("ctrl-g", DoSomething, Some("my_context")),
    ]);
});

// Dispatch action directly
cx.dispatch_action(*window, DoSomething);
```

## Testing Events (EventEmitter)

### Collecting events with `cx.events()`

```rust
use futures::channel::mpsc::UnboundedReceiver;

let mut events: UnboundedReceiver<MyEvent> = cx.events(&entity);

// Trigger something that emits an event
entity.update(cx, |state, cx| {
    state.do_action(cx); // calls cx.emit(MyEvent::Happened)
});

// Check the event was emitted
assert_eq!(events.try_next().unwrap(), Some(MyEvent::Happened));
```

### Waiting for a single event

```rust
let event = entity.next_event(cx).await;
assert_eq!(event, MyEvent::Completed);
```

### Manual subscription with shared state

```rust
use std::cell::RefCell;
use std::rc::Rc;

let events = Rc::new(RefCell::new(Vec::new()));
let events_clone = events.clone();

entity.update(cx, |_, cx| {
    cx.subscribe(&entity, move |_, _, event, _cx| {
        events_clone.borrow_mut().push(event.clone());
    }).detach();
});

// ... perform actions ...

assert_eq!(*events.borrow(), vec![MyEvent::A, MyEvent::B]);
```

## Testing Notifications (cx.notify())

```rust
let mut notifications = cx.notifications(&entity);

entity.update(cx, |state, cx| {
    state.value = 42;
    cx.notify();
});

// Verify notification was sent
assert!(notifications.try_next().is_ok());
```

## Waiting for Conditions

For async tests where state changes happen through background work:

```rust
// Wait until a predicate becomes true
entity.condition(cx, |state, _cx| state.loading_complete).await;

// Run all pending background tasks to completion
cx.run_until_parked();

// Advance simulated clock (for timer/timeout testing)
cx.advance_clock(Duration::from_secs(5));
```

## Drawing Elements for Layout Testing

Test element layout without a full view:

```rust
let cx = cx.add_empty_window();

cx.draw(
    point(px(0.), px(0.)),
    size(px(800.), px(600.)),
    |window, cx| {
        div()
            .w(px(200.))
            .h(px(100.))
            .debug_selector(|| "my-element".to_string())
            .child("Hello")
    },
);

// Query rendered bounds
let bounds = cx.debug_bounds("my-element");
assert_eq!(bounds.unwrap().size.width, px(200.));
```

## Platform Mocks

The test framework automatically provides a `TestPlatform` with:

- **Clipboard**: `write_to_clipboard` / `read_from_clipboard` work in-memory
- **File dialogs**: Use `cx.simulate_new_path_selection(path)` to mock file picker results
- **Prompts**: Use `cx.simulate_prompt_answer(index)` for alert dialogs
- **URLs**: Track opened URLs via platform mock

## Deterministic Execution

`TestDispatcher` provides deterministic task scheduling:

- All spawned tasks are queued, not run immediately
- `cx.run_until_parked()` drains all ready tasks
- Seeded RNG controls task interleaving order (useful for race condition testing)
- `cx.advance_clock(duration)` moves simulated time forward for timeout testing

## Complete Example

```rust
use gpui::*;

struct Counter {
    value: i32,
    focus_handle: FocusHandle,
}

#[derive(Clone, Debug, PartialEq)]
enum CounterEvent {
    Changed(i32),
}

impl EventEmitter<CounterEvent> for Counter {}

actions!(counter, [Increment, Decrement]);

impl Counter {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            value: 0,
            focus_handle: cx.focus_handle(),
        }
    }

    fn increment(&mut self, cx: &mut Context<Self>) {
        self.value += 1;
        cx.emit(CounterEvent::Changed(self.value));
        cx.notify();
    }
}

impl Render for Counter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("counter")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Increment, _, cx| {
                this.increment(cx);
            }))
            .child(format!("Count: {}", self.value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn test_increment(cx: &mut TestAppContext) {
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| Counter::new(cx))
            }).unwrap()
        });

        cx.update(|cx| {
            cx.bind_keys(vec![
                KeyBinding::new("ctrl-up", Increment, Some("counter")),
            ]);
        });

        // Focus the view
        window.update(cx, |counter, window, _cx| {
            window.focus(&counter.focus_handle);
        }).unwrap();

        // Verify initial state
        window.update(cx, |counter, _, _| {
            assert_eq!(counter.value, 0);
        }).unwrap();

        // Simulate keystroke
        cx.simulate_keystrokes("ctrl-up");

        // Verify state changed
        window.update(cx, |counter, _, _| {
            assert_eq!(counter.value, 1);
        }).unwrap();
    }

    #[gpui::test]
    fn test_increment_emits_event(cx: &mut TestAppContext) {
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| Counter::new(cx))
            }).unwrap()
        });

        let mut events = cx.events(&window);

        window.update(cx, |counter, _, cx| {
            counter.increment(cx);
        }).unwrap();

        assert_eq!(
            events.try_next().unwrap(),
            Some(CounterEvent::Changed(1))
        );
    }
}
```

## Tips for Testable Components

1. **Separate data from rendering** — put mutation logic in methods on the struct so tests
   can call them directly without simulating UI interactions.

2. **Use EventEmitter** — emit events for state changes so tests can assert on them
   without inspecting internal state.

3. **Accept dependencies as parameters** — instead of reaching into globals, pass
   collaborators via constructor so tests can provide mocks.

4. **Keep `Render` thin** — complex layout logic can be tested via `cx.draw()` +
   `debug_bounds()`, but it's easier to test state transitions on the struct directly.

5. **Use `cx.notify()` consistently** — this enables `cx.notifications(&entity)` in tests
   to detect when re-renders would be triggered.
