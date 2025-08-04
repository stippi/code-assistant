# GPUI Mouse Events Guide

This document provides a comprehensive overview of mouse event handling in GPUI, based on analysis of the Zed project's GPUI implementation.

## Available Mouse Events

GPUI provides several mouse event types, all defined in `interactive.rs`:

1. **MouseDownEvent** - When a mouse button is pressed
2. **MouseUpEvent** - When a mouse button is released  
3. **MouseMoveEvent** - When the mouse is moved
4. **ScrollWheelEvent** - When the mouse wheel is scrolled
5. **MouseExitEvent** - When the mouse leaves the window
6. **ClickEvent** - A composite event combining MouseDown + MouseUp
7. **FileDropEvent** - When files are dragged and dropped

## Mouse Buttons Supported

The `MouseButton` enum supports:
- `Left` - Left mouse button
- `Right` - Right mouse button  
- `Middle` - Middle mouse button
- `Navigate(NavigationDirection)` - Back/Forward navigation buttons

## Event Handler Methods Available

### InteractiveElement Trait Methods

**Mouse Down Events:**
- `on_mouse_down(button, listener)` - Listen for specific button press (bubble phase)
- `on_any_mouse_down(listener)` - Listen for any button press (bubble phase)  
- `capture_any_mouse_down(listener)` - Listen for any button press (capture phase)
- `on_mouse_down_out(listener)` - Listen for mouse down outside element bounds

**Mouse Up Events:**
- `on_mouse_up(button, listener)` - Listen for specific button release (bubble phase)
- `capture_any_mouse_up(listener)` - Listen for any button release (capture phase)
- `on_mouse_up_out(button, listener)` - Listen for mouse up outside element bounds

**Other Mouse Events:**
- `on_mouse_move(listener)` - Listen for mouse movement over element
- `on_scroll_wheel(listener)` - Listen for scroll wheel events
- `on_drag_move<T>(listener)` - Listen for drag move events of specific type

### StatefulInteractiveElement Trait Methods

**StatefulInteractiveElement** trait adds:
- `on_click(listener)` - Listen for click events (mouse down + up)
- `on_hover(listener)` - Listen for hover start/end events  
- `on_drag<T, W>(value, constructor)` - Handle drag initiation

## Event Phases

GPUI uses a two-phase event system:

1. **Capture Phase** (`DispatchPhase::Capture`) - Events flow from root to target
2. **Bubble Phase** (`DispatchPhase::Bubble`) - Events flow from target back to root

## Mouse Up Event Handling

To be notified of "mouse up" events, you have several options:

### For Specific Button Release
```rust
element.on_mouse_up(MouseButton::Left, |event, window, cx| {
    // Handle left mouse button release
    println!("Left mouse button released at {:?}", event.position);
})
```

### For Any Button Release (Capture Phase)
```rust
element.capture_any_mouse_up(|event, window, cx| {
    // Handle any mouse button release during capture phase
    println!("Mouse button released: {:?}", event.button);
})
```

### For Mouse Up Outside Element
```rust
element.on_mouse_up_out(MouseButton::Left, |event, window, cx| {
    // Handle left mouse button release outside element bounds
})
```

## Event Properties

### MouseUpEvent Properties
- `button: MouseButton` - Which button was released
- `position: Point<Pixels>` - Mouse position in window coordinates  
- `modifiers: Modifiers` - Keyboard modifiers held during release
- `click_count: usize` - Number of consecutive clicks

### MouseDownEvent Properties
- `button: MouseButton` - Which button was pressed
- `position: Point<Pixels>` - Mouse position in window coordinates
- `modifiers: Modifiers` - Keyboard modifiers held during press
- `click_count: usize` - Number of consecutive clicks
- `first_mouse: bool` - Whether this is the first, focusing click

### MouseMoveEvent Properties
- `position: Point<Pixels>` - Current mouse position
- `pressed_button: Option<MouseButton>` - Button held during move (if any)
- `modifiers: Modifiers` - Keyboard modifiers
- `dragging()` method - Returns true if left button is pressed

### ScrollWheelEvent Properties
- `position: Point<Pixels>` - Mouse position during scroll
- `delta: ScrollDelta` - Scroll amount (pixels or lines)
- `modifiers: Modifiers` - Keyboard modifiers
- `touch_phase: TouchPhase` - Touch phase for trackpad scrolling

## Common Usage Patterns

### Basic Click Handling
```rust
div()
    .on_click(|event, window, cx| {
        println!("Clicked at {:?}", event.up.position);
    })
```

### Drag and Drop
```rust
div()
    .on_mouse_down(MouseButton::Left, |event, window, cx| {
        // Start drag operation
    })
    .on_mouse_move(|event, window, cx| {
        if event.dragging() {
            // Handle drag movement
        }
    })
    .on_mouse_up(MouseButton::Left, |event, window, cx| {
        // End drag operation
    })
```

### Hover Effects
```rust
div()
    .on_hover(|is_hovered, window, cx| {
        if *is_hovered {
            println!("Mouse entered element");
        } else {
            println!("Mouse left element");
        }
    })
```

### Right-Click Context Menu
```rust
div()
    .on_mouse_up(MouseButton::Right, |event, window, cx| {
        // Show context menu at event.position
    })
```

## InteractiveText

For text elements, GPUI provides `InteractiveText` which offers:

- `on_click(ranges, listener)` - Handle clicks on specific text ranges
- `on_hover(listener)` - Handle hover over individual characters
- `tooltip(builder)` - Show tooltips for specific character positions

### InteractiveText Usage
```rust
InteractiveText::new(id, styled_text)
    .on_click(clickable_ranges, |range_index, window, cx| {
        println!("Clicked on range {}", range_index);
    })
    .on_hover(|char_index, event, window, cx| {
        if let Some(index) = char_index {
            println!("Hovering over character {}", index);
        }
    })
```

## Notes

- All mouse event handlers receive `&mut Window` and `&mut App` parameters for accessing application state
- Event propagation can be controlled using `cx.stop_propagation()`
- Mouse events respect the element's hitbox and styling (e.g., `pointer-events: none` equivalent)
- The coordinate system uses `Point<Pixels>` with the origin at the top-left of the window
