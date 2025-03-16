use gpui::{div, prelude::*, px, rgb, AnyElement, Pixels, Point, ScrollHandle};
use std::{cell::RefCell, rc::Rc};

/// Default width of the scrollbar thumb
const DEFAULT_SCROLLBAR_THUMB_WIDTH: Pixels = px(8.);
/// Minimum height of the scrollbar thumb
const MIN_SCROLLBAR_THUMB_HEIGHT: Pixels = px(30.);
/// Padding from the edge of the container
const SCROLLBAR_PADDING: Pixels = px(4.);

/// A handle for controlling and tracking the state of a scrollbar.
#[derive(Clone, Debug)]
pub struct ScrollbarHandle {
    /// Shared state for the scrollbar
    pub state: Rc<RefCell<ScrollbarState>>,
}

/// Internal state tracked by the scrollbar
#[derive(Clone, Debug)]
pub struct ScrollbarState {
    /// The underlying scroll handle from GPUI
    pub scroll_handle: ScrollHandle,
    /// The position within the thumb where dragging started
    pub drag_position: Option<Point<Pixels>>,
    /// Whether the mouse is hovering over the scrollbar
    pub is_hovered: bool,
}

/// Configuration for the scrollbar appearance
#[derive(Clone, Debug)]
pub struct ScrollbarStyle {
    /// Width of the scrollbar thumb
    pub thumb_width: Pixels,
    /// Default color of the thumb
    pub thumb_color: gpui::Rgba,
    /// Color of the thumb when hovered
    pub thumb_hover_color: gpui::Rgba,
    /// Border radius of the thumb
    pub border_radius: Pixels,
}

impl Default for ScrollbarStyle {
    fn default() -> Self {
        Self {
            thumb_width: DEFAULT_SCROLLBAR_THUMB_WIDTH,
            thumb_color: rgb(0xC0C0C0),
            thumb_hover_color: rgb(0xA0A0A0),
            border_radius: px(4.),
        }
    }
}

impl ScrollbarHandle {
    /// Create a new scrollbar handle
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(ScrollbarState {
                scroll_handle: ScrollHandle::new(),
                drag_position: None,
                is_hovered: false,
            })),
        }
    }

    /// Get the current scroll offset
    pub fn offset(&self) -> Point<Pixels> {
        self.state.borrow().scroll_handle.offset()
    }

    /// Set the scroll offset
    pub fn set_offset(&self, offset: Point<Pixels>) {
        self.state.borrow_mut().scroll_handle.set_offset(offset);
    }
}

/// Simple vertical scrollbar component
pub struct Scrollbar {
    /// Handle to control the scrollbar state
    pub handle: ScrollbarHandle,
    /// Style configuration for the scrollbar
    pub style: ScrollbarStyle,
}

impl Scrollbar {
    /// Create a new scrollbar with default style
    pub fn new() -> Self {
        Self {
            handle: ScrollbarHandle::new(),
            style: ScrollbarStyle::default(),
        }
    }

    /// Create a new scrollbar with custom style
    pub fn with_style(style: ScrollbarStyle) -> Self {
        Self {
            handle: ScrollbarHandle::new(),
            style,
        }
    }

    /// Use an existing handle for the scrollbar
    pub fn with_handle(mut self, handle: ScrollbarHandle) -> Self {
        self.handle = handle;
        self
    }

    /// Render the scrollbar as an element
    pub fn render(&self) -> AnyElement {
        let style = self.style.clone();

        // Simplified scrollbar visualization
        // In a real implementation, we'd calculate position based on viewport/content ratio
        div()
            .absolute()
            .right(SCROLLBAR_PADDING)
            .top(px(100.)) // Fixed position for simplicity
            .h(MIN_SCROLLBAR_THUMB_HEIGHT)
            .w(style.thumb_width)
            .bg(style.thumb_color)
            .rounded(style.border_radius)
            .into_any()
    }
}

impl IntoElement for Scrollbar {
    type Element = AnyElement;

    fn into_element(self) -> Self::Element {
        self.render()
    }
}

/// Create a container with a custom scrollbar
pub struct ScrollableContainer<E: IntoElement> {
    /// The content to be made scrollable
    content: E,
    /// Handle to control the scrollbar
    handle: ScrollbarHandle,
    /// Optional custom style for the scrollbar
    style: Option<ScrollbarStyle>,
}

impl<E: IntoElement> ScrollableContainer<E> {
    /// Create a new scrollable container
    pub fn new(content: E) -> Self {
        Self {
            content,
            handle: ScrollbarHandle::new(),
            style: None,
        }
    }

    /// Use a specific handle for the scrollbar
    pub fn with_handle(mut self, handle: ScrollbarHandle) -> Self {
        self.handle = handle;
        self
    }

    /// Apply custom styling to the scrollbar
    pub fn with_style(mut self, style: ScrollbarStyle) -> Self {
        self.style = Some(style);
        self
    }

    /// Get the scrollbar handle
    pub fn handle(&self) -> ScrollbarHandle {
        self.handle.clone()
    }
}

impl<E: IntoElement> IntoElement for ScrollableContainer<E> {
    type Element = AnyElement;

    fn into_element(self) -> Self::Element {
        let scrollbar = Scrollbar {
            handle: self.handle,
            style: self.style.unwrap_or_default(),
        };

        div()
            .relative()
            .size_full()
            .overflow_y_hidden()
            .child(self.content)
            .child(scrollbar)
            .into_any()
    }
}

/// Extension trait to add scrollbar functionality to containers
pub trait ScrollableExt: IntoElement + Sized {
    /// Add a custom scrollbar to a scrollable element
    fn with_scrollbar(self) -> ScrollableContainer<Self> {
        ScrollableContainer::new(self)
    }

    /// Add a scrollbar with a specific handle
    fn with_scrollbar_handle(self, handle: ScrollbarHandle) -> ScrollableContainer<Self> {
        ScrollableContainer::new(self).with_handle(handle)
    }

    /// Add a styled scrollbar
    fn with_styled_scrollbar(self, style: ScrollbarStyle) -> ScrollableContainer<Self> {
        ScrollableContainer::new(self).with_style(style)
    }
}

// Implement the extension trait for all elements
impl<T: IntoElement> ScrollableExt for T {}

// Convenience function to create a scrollbar with default style
pub fn scrollbar() -> Scrollbar {
    Scrollbar::new()
}

// Convenience function to create a scrollbar with custom style
pub fn styled_scrollbar(style: ScrollbarStyle) -> Scrollbar {
    Scrollbar::with_style(style)
}
