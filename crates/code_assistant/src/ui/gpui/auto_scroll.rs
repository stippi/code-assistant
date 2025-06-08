use gpui::{
    div, prelude::*, px, Bounds, Context, Entity, Pixels, Point, ScrollHandle, SharedString, Size,
    Task, Timer, Window,
};
use gpui_component::scroll::{Scrollbar, ScrollbarState};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use tracing::{debug, trace};

/// Configuration for auto-scroll behavior
#[derive(Clone)]
pub struct AutoScrollConfig {
    /// Tolerance in pixels for "at bottom" detection
    pub bottom_tolerance: Pixels,
    /// Animation frame rate (in milliseconds per frame)
    pub animation_frame_ms: u64,
    /// Spring constant for the spring-damper animation
    pub spring_k: f32,
    /// Damping constant for the spring-damper animation
    pub damping_c: f32,
    /// Minimum distance to stop scrolling (in pixels)
    pub min_distance_to_stop: f32,
    /// Minimum speed to stop scrolling
    pub min_speed_to_stop: f32,
}

impl Default for AutoScrollConfig {
    fn default() -> Self {
        Self {
            bottom_tolerance: px(50.0),
            animation_frame_ms: 8, // ~120 FPS
            spring_k: 0.035,
            damping_c: 0.32,
            min_distance_to_stop: 0.5,
            min_speed_to_stop: 0.5,
        }
    }
}

/// AutoScrollContainer - A reusable component that wraps scrollable content with auto-scroll functionality
pub struct AutoScrollContainer<T: Render> {
    // Core scroll state
    scroll_handle: ScrollHandle,
    scrollbar_state: Rc<Cell<ScrollbarState>>,
    content_size: Rc<Cell<Size<Pixels>>>,
    viewport_size: Rc<Cell<Size<Pixels>>>,

    // Auto-scroll state
    autoscroll_active: Rc<Cell<bool>>,
    was_at_bottom_before_update: Rc<Cell<bool>>,
    autoscroll_task: Rc<RefCell<Option<Task<()>>>>,

    // Content change detection
    last_content_height: Rc<Cell<f32>>,

    // Configuration
    config: AutoScrollConfig,

    // Content ID for tracking
    content_id: String,

    // Content entity
    content_entity: Entity<T>,
}

impl<T: Render> AutoScrollContainer<T> {
    /// Create a new AutoScrollContainer with default configuration
    pub fn new(content_id: impl Into<String>, content_entity: Entity<T>) -> Self {
        Self::with_config(content_id, content_entity, AutoScrollConfig::default())
    }

    /// Create a new AutoScrollContainer with custom configuration
    pub fn with_config(
        content_id: impl Into<String>,
        content_entity: Entity<T>,
        config: AutoScrollConfig,
    ) -> Self {
        Self {
            scroll_handle: ScrollHandle::new(),
            scrollbar_state: Rc::new(Cell::new(ScrollbarState::default())),
            content_size: Rc::new(Cell::new(Size::default())),
            viewport_size: Rc::new(Cell::new(Size::default())),
            autoscroll_active: Rc::new(Cell::new(false)),
            was_at_bottom_before_update: Rc::new(Cell::new(false)),
            autoscroll_task: Rc::new(RefCell::new(None)),
            last_content_height: Rc::new(Cell::new(0.0)),
            config,
            content_id: content_id.into(),
            content_entity,
        }
    }

    /// Get the scroll handle for external access if needed
    pub fn scroll_handle(&self) -> &ScrollHandle {
        &self.scroll_handle
    }

    /// Get the current content size
    pub fn content_size(&self) -> Size<Pixels> {
        self.content_size.get()
    }

    /// Get the current viewport size
    pub fn viewport_size(&self) -> Size<Pixels> {
        self.viewport_size.get()
    }

    /// Check if auto-scroll is currently active
    pub fn is_autoscroll_active(&self) -> bool {
        self.autoscroll_active.get()
    }

    /// Manually trigger auto-scroll (useful for programmatic scrolling)
    pub fn trigger_autoscroll(&self, cx: &mut Context<Self>) {
        self.autoscroll_active.set(true);
        self.start_autoscroll_task(cx);
    }

    /// Check if we're currently at the bottom of the scroll area
    fn is_at_bottom(&self, tolerance: Pixels) -> bool {
        let current_scroll_offset_y = self.scroll_handle.offset().y;
        let content_height = self.content_size.get().height;
        let viewport_height = self.viewport_size.get().height;

        // If content is smaller than or equal to viewport, we are always "at bottom"
        if content_height <= viewport_height {
            return true;
        }

        // Max scroll offset is -(content_height - viewport_height)
        let max_scroll_offset_y = -(content_height - viewport_height);

        // Check if current offset is within tolerance of max scroll offset
        (current_scroll_offset_y - max_scroll_offset_y).abs() <= tolerance
    }

    /// Start the auto-scroll animation task
    fn start_autoscroll_task(&self, cx: &mut Context<Self>) {
        // Cancel existing task if any
        *self.autoscroll_task.borrow_mut() = None;

        if !self.autoscroll_active.get() {
            trace!("Auto-scroll not active, task not started.");
            return;
        }
        debug!("Starting autoscroll task...");

        let scroll_handle_orig = self.scroll_handle.clone();
        let autoscroll_active_orig = self.autoscroll_active.clone();
        let content_size_rc = self.content_size.clone();
        let viewport_size_rc = self.viewport_size.clone();
        let config = self.config.clone();

        let task = cx.spawn(async move |weak_entity, async_app_cx| {
            let mut timer = Timer::after(Duration::from_millis(config.animation_frame_ms));

            // Easing animation variables
            let mut current_scroll_speed: f32 = 0.0;

            loop {
                timer.await;
                timer = Timer::after(Duration::from_millis(config.animation_frame_ms));

                let autoscroll_active_for_update = autoscroll_active_orig.clone();
                let scroll_handle_for_update = scroll_handle_orig.clone();
                let content_size_for_update = content_size_rc.clone();
                let viewport_size_for_update = viewport_size_rc.clone();

                let update_result = weak_entity.update(async_app_cx, move |_view, model_cx| {
                    if !autoscroll_active_for_update.get() {
                        return false; // Stop task
                    }

                    // Use the stored sizes instead of accessing view directly
                    let content_h = content_size_for_update.get().height;
                    let viewport_h = viewport_size_for_update.get().height;

                    if viewport_h == px(0.0) {
                        return true; // Viewport not measured yet, wait
                    }

                    let scrollable_amount = content_h - viewport_h;
                    let target_y_px = if scrollable_amount > px(0.0) {
                        -scrollable_amount
                    } else {
                        px(0.0)
                    };

                    let current_offset_y_px = scroll_handle_for_update.offset().y;
                    let displacement_x_f32 = current_offset_y_px.0 - target_y_px.0;
                    let distance_to_target_abs_f32 = displacement_x_f32.abs();

                    if distance_to_target_abs_f32 < config.min_distance_to_stop
                        && current_scroll_speed.abs() < config.min_speed_to_stop
                    {
                        scroll_handle_for_update.set_offset(Point {
                            x: px(0.0),
                            y: target_y_px,
                        });
                        autoscroll_active_for_update.set(false);
                        return false; // Stop task
                    }

                    let force_spring_f32 = -config.spring_k * displacement_x_f32;
                    let force_damping_f32 = -config.damping_c * current_scroll_speed;
                    let total_acceleration_f32 = force_spring_f32 + force_damping_f32;
                    current_scroll_speed += total_acceleration_f32;

                    let mut final_scroll_delta_f32 = current_scroll_speed;

                    if displacement_x_f32.abs() > f32::EPSILON {
                        let current_displacement_sign = displacement_x_f32.signum();
                        let planned_offset_y_f32 = current_offset_y_px.0 + final_scroll_delta_f32;
                        let planned_displacement_f32 = planned_offset_y_f32 - target_y_px.0;

                        if planned_displacement_f32.signum() != current_displacement_sign {
                            if distance_to_target_abs_f32 > config.min_distance_to_stop {
                                final_scroll_delta_f32 = -displacement_x_f32;
                            }
                        }
                    }

                    if final_scroll_delta_f32.abs() > distance_to_target_abs_f32 {
                        final_scroll_delta_f32 = -displacement_x_f32;
                    }

                    let new_y_calculated_px = current_offset_y_px + px(final_scroll_delta_f32);

                    scroll_handle_for_update.set_offset(Point {
                        x: px(0.0),
                        y: new_y_calculated_px,
                    });
                    model_cx.notify();
                    true // Continue task
                });

                if update_result.is_err() || !update_result.unwrap_or(false) {
                    autoscroll_active_orig.set(false);
                    break;
                }
            }
        });

        *self.autoscroll_task.borrow_mut() = Some(task);
    }

    /// Handle content size changes (detecting new content)
    pub fn handle_content_change(&self, new_height: f32, cx: &mut Context<Self>) {
        let old_height = self.last_content_height.get();
        self.last_content_height.set(new_height);

        // Check if at bottom BEFORE considering new height for auto-scroll decision logic
        let at_bottom_before_update = self.is_at_bottom(self.config.bottom_tolerance);
        self.was_at_bottom_before_update
            .set(at_bottom_before_update);
        trace!(
            "ContentChange: was_at_bottom_before_update set to: {}",
            at_bottom_before_update
        );

        // Content grew (new content added)
        if new_height > old_height + 1.0 {
            // Decide if we need to autoscroll based on new logic
            if self.was_at_bottom_before_update.get() || self.autoscroll_active.get() {
                self.autoscroll_active.set(true);
                trace!("ContentChange: autoscroll_active set to true, starting task.");
                self.start_autoscroll_task(cx);
            } else {
                self.autoscroll_active.set(false);
                trace!("ContentChange: autoscroll_active set to false.");
            }
        }
    }
}

impl<T: Render> Render for AutoScrollContainer<T> {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().clone();

        // Clone handles for closures
        let content_size_rc = self.content_size.clone();
        let viewport_size_rc = self.viewport_size.clone();
        let content_entity = self.content_entity.clone();

        div()
            .on_children_prepainted({
                // Listener for viewport_size
                let viewport_size_rc = viewport_size_rc.clone();
                move |bounds_vec: Vec<Bounds<Pixels>>, _window, _app| {
                    if let Some(first_child_bounds) = bounds_vec.first() {
                        let new_viewport_size = first_child_bounds.size;
                        if viewport_size_rc.get() != new_viewport_size {
                            trace!("viewport size changed: {:?}", new_viewport_size);
                            viewport_size_rc.set(new_viewport_size);
                        }
                    }
                }
            })
            .id(SharedString::new(format!("{}-container", self.content_id)))
            .flex_1() // Take remaining space in the parent container
            .min_h_0() // Minimum height to ensure scrolling works
            .relative() // For absolute positioning of scrollbar
            .overflow_hidden() // Crucial for stable viewport measurement
            .child(
                // Child 1: The actual scrolling viewport
                div()
                    .id(SharedString::new(format!(
                        "{}-scroll-container",
                        self.content_id
                    )))
                    .size_full() // Fills the container
                    .overflow_scroll() // Enables native scrolling for this div
                    .track_scroll(&self.scroll_handle) // Links to our scroll state
                    .child(
                        // Wrapper for content to measure their content size
                        div()
                            .on_children_prepainted({
                                let view_entity = view.clone();
                                move |bounds_vec, _window, app| {
                                    if let Some(text_view_bounds) = bounds_vec.first() {
                                        let new_content_size = text_view_bounds.size;
                                        view_entity.update(app, |view, cx_update| {
                                            if view.content_size != new_content_size {
                                                view.content_size = new_content_size;
                                                println!(
                                                    "New content_size: {:?}",
                                                    new_content_size
                                                );
                                                cx_update.notify();
                                            }
                                        });
                                    }
                                }
                            })
                            .id(SharedString::new(format!(
                                "{}-content-wrapper",
                                self.content_id
                            )))
                            .w_full() // Important for correct height calculation
                            .child(content_entity.clone()), // Use the content entity
                    ),
            )
            .child(
                // Child 2: The manual scrollbar, absolutely positioned
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(12.))
                    .child(Scrollbar::vertical(
                        view.entity_id(),
                        self.scrollbar_state.clone(),
                        self.scroll_handle.clone(),
                        self.content_size.get(),
                    )),
            )
    }
}
