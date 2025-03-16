use gpui::{
    canvas, div, prelude::*, px, rgb, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    Window,
};

/// Width of the scrollbar thumb
const DEFAULT_SCROLLBAR_THUMB_WIDTH: Pixels = px(8.);
/// Minimum height of the scrollbar thumb
const MIN_SCROLLBAR_THUMB_HEIGHT: Pixels = px(30.);
/// Padding from the edge of the container
const SCROLLBAR_PADDING: Pixels = px(4.);

/// Configuration for the scrollbar appearance
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

/// State for the scrollbar
#[derive(Default)]
pub struct Scrollbar {
    /// The style of the scrollbar
    style: ScrollbarStyle,
    /// The position in thumb bounds when dragging starts (mouse down)
    drag_position: Option<Point<Pixels>>,
    /// Flag to indicate if the mouse is currently hovering over the thumb
    is_hovered: bool,
}

impl Scrollbar {
    pub fn new() -> Self {
        Self {
            style: ScrollbarStyle::default(),
            drag_position: None,
            is_hovered: false,
        }
    }

    /// Set a custom style for the scrollbar
    pub fn with_style(mut self, style: ScrollbarStyle) -> Self {
        self.style = style;
        self
    }

    /// Get the current thumb color based on hover state
    fn current_thumb_color(&self) -> gpui::Rgba {
        if self.is_hovered {
            self.style.thumb_hover_color
        } else {
            self.style.thumb_color
        }
    }

    /// Calculate the height of the scrollbar thumb based on content and viewport
    fn calculate_thumb_height(&self, viewport_height: Pixels, content_height: Pixels) -> Pixels {
        if content_height <= viewport_height {
            viewport_height // Full height if no scrolling needed
        } else {
            // Calculate proportional thumb height
            let ratio = viewport_height / content_height;
            let height = viewport_height * ratio;
            // Ensure minimum height
            height.max(MIN_SCROLLBAR_THUMB_HEIGHT)
        }
    }

    /// Calculate the vertical position of the thumb based on scroll position
    fn calculate_thumb_position(
        &self,
        viewport_height: Pixels,
        content_height: Pixels,
        scroll_top: Pixels,
        thumb_height: Pixels,
    ) -> Pixels {
        if content_height <= viewport_height {
            px(0.) // No scrolling needed, position at top
        } else {
            // Calculate position based on scroll percentage
            let max_scroll = content_height - viewport_height;
            let scroll_percentage = (-scroll_top / max_scroll).clamp(0., 1.);
            let max_thumb_offset = viewport_height - thumb_height;
            (max_thumb_offset * scroll_percentage).clamp(px(0.), max_thumb_offset)
        }
    }
}

impl Render for Scrollbar {
    /// Render a scrollbar at the right edge of a scrollable view
    ///
    /// # Arguments
    /// * `cx` - The context
    /// * `scroll_top` - The current vertical scroll position (y-offset)
    /// * `view_height` - The height of the viewport
    /// * `content_height` - The height of the scrollable content
    /// * `scroll_callback` - Called when the scrollbar is dragged to update scroll position
    fn render<F>(
        &mut self,
        cx: &mut Window,
        scroll_top: Pixels,
        view_height: Pixels,
        content_height: Pixels,
        scroll_callback: F,
    ) -> impl IntoElement
    where
        F: Fn(Pixels) + 'static,
    {
        // Don't render scrollbar if content fits within viewport
        if content_height <= view_height {
            return div().id("scrollbar-hidden");
        }

        let entity_id = cx.entity_id();
        let thumb_height = self.calculate_thumb_height(view_height, content_height);
        let thumb_position =
            self.calculate_thumb_position(view_height, content_height, scroll_top, thumb_height);

        // Current mouse hover state
        let is_hovered = self.is_hovered;

        div()
            .id("scrollbar-container")
            .absolute()
            .right(SCROLLBAR_PADDING)
            .top(thumb_position + SCROLLBAR_PADDING)
            .w(self.style.thumb_width)
            .h(thumb_height)
            .bg(if is_hovered {
                self.style.thumb_hover_color
            } else {
                self.style.thumb_color
            })
            .rounded(self.style.border_radius)
            .child(
                canvas({
                    let entity_id = entity_id;
                    move |bounds, _, window, cx| {
                        // Handle mouse down on thumb
                        window.on_mouse_event({
                            let entity_id = entity_id;
                            move |ev: &MouseDownEvent, _, _, cx| {
                                if !bounds.contains(&ev.position) {
                                    return;
                                }

                                cx.update_entity(entity_id, |this: &mut Scrollbar, _| {
                                    this.drag_position = Some(ev.position - bounds.origin);
                                });
                            }
                        });

                        // Handle mouse up to end dragging
                        window.on_mouse_event({
                            let entity_id = entity_id;
                            move |_: &MouseUpEvent, _, _, cx| {
                                cx.update_entity(entity_id, |this: &mut Scrollbar, _| {
                                    this.drag_position = None;
                                });
                            }
                        });

                        // Handle mouse movement for dragging and hover
                        window.on_mouse_event({
                            let entity_id = entity_id;
                            let content_height = content_height;
                            let view_height = view_height;
                            let scroll_callback = scroll_callback.clone();

                            move |ev: &MouseMoveEvent, _, _, cx| {
                                // Update hover state
                                let is_hovering = bounds.contains(&ev.position);
                                cx.update_entity(entity_id, |this: &mut Scrollbar, cx| {
                                    if this.is_hovered != is_hovering {
                                        this.is_hovered = is_hovering;
                                        cx.notify();
                                    }
                                });

                                // If not dragging, we're done
                                if !ev.dragging() {
                                    return;
                                }

                                cx.update_entity(entity_id, |this: &mut Scrollbar, _| {
                                    if let Some(drag_pos) = this.drag_position {
                                        // Calculate position based on mouse
                                        let window_bounds = cx.bounds();
                                        let max_offset = view_height - bounds.size.height;
                                        let position_y =
                                            ev.position.y - window_bounds.origin.y - drag_pos.y;
                                        let percentage = (position_y / max_offset).clamp(0.0, 1.0);

                                        // Calculate new scroll position
                                        let max_scroll = content_height - view_height;
                                        let new_scroll_y = -(max_scroll * percentage);

                                        // Notify via callback
                                        scroll_callback(new_scroll_y);
                                    }
                                });
                            }
                        });
                    }
                })
                .size_full(),
            )
    }
}

/// Constructor for a custom style scrollbar
pub fn styled_scrollbar(style: ScrollbarStyle) -> Scrollbar {
    Scrollbar::new().with_style(style)
}

/// Create a scrollbar with the default style
pub fn scrollbar() -> Scrollbar {
    Scrollbar::new()
}
