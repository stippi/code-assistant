use super::elements::MessageContainer;
use gpui::{
    bounce, div, ease_in_out, percentage, prelude::*, px, rgb, svg, Animation, AnimationExt, App,
    Bounds, Context, Entity, FocusHandle, Focusable, Pixels, Point, ScrollHandle, SharedString,
    Size, Task, Timer, Transformation, Window,
};
use gpui_component::{
    scroll::{Scrollbar, ScrollbarState},
    v_flex, ActiveTheme,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration; // Instant is no longer used here directly for AutoScrollState

// AutoScrollConfig and AutoScrollState structs are removed.

/// MessagesView - Component responsible for displaying the message history
pub struct MessagesView {
    message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
    focus_handle: FocusHandle,

    // Auto-scroll functionality
    scroll_handle: ScrollHandle,
    // scroll_state and config are removed
    scrollbar_state: Rc<Cell<ScrollbarState>>,
    content_size: Rc<Cell<Size<Pixels>>>,
    viewport_size: Rc<Cell<Size<Pixels>>>,
    autoscroll_active: Rc<Cell<bool>>,
    was_at_bottom_before_update: Rc<Cell<bool>>,

    // Animation task (renamed from scroll_task for consistency with playground)
    autoscroll_task: Rc<RefCell<Option<Task<()>>>>,

    // Track content size to detect content changes
    last_content_height: Rc<Cell<f32>>,
}

impl MessagesView {
    pub fn new(
        message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            message_queue,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            // scroll_state and config initializations are removed
            scrollbar_state: Rc::new(Cell::new(ScrollbarState::default())),
            content_size: Rc::new(Cell::new(Size::default())),
            viewport_size: Rc::new(Cell::new(Size::default())),
            autoscroll_active: Rc::new(Cell::new(false)), // Initialize to false
            was_at_bottom_before_update: Rc::new(Cell::new(false)), // Initialize to false
            autoscroll_task: Rc::new(RefCell::new(None)),
            last_content_height: Rc::new(Cell::new(0.0)),
        }
    }

    // is_at_bottom will be replaced by the version from gpui-playground
    // start_auto_scroll_animation will be replaced by start_autoscroll_task from gpui-playground

    // /// Check if we're currently at the bottom of the scroll area
    // Ported from gpui-playground
    fn is_at_bottom(&self, tolerance: Pixels) -> bool {
        let current_scroll_offset_y = self.scroll_handle.offset().y;
        // content_size and viewport_size are Rc<Cell<Size<Pixels>>> in MessagesView
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

    // Ported from gpui-playground and adapted for MessagesView
    fn start_autoscroll_task(&self, cx: &mut Context<Self>) {
        // Cancel existing task if any
        *self.autoscroll_task.borrow_mut() = None; // Use the renamed task field

        if !self.autoscroll_active.get() {
            // println!("Auto-scroll not active, task not started.");
            return;
        }
        // println!("Starting autoscroll task...");

        let scroll_handle_orig = self.scroll_handle.clone();
        let autoscroll_active_orig = self.autoscroll_active.clone();

        let task = cx.spawn(async move |weak_entity, async_app_cx| {
            let mut timer = Timer::after(Duration::from_millis(16)); // Aim for ~60 FPS

            // Easing animation variables
            let mut current_scroll_speed: f32 = 0.0;
            const SPRING_K: f32 = 0.035;
            const DAMPING_C: f32 = 0.32;
            const MIN_DISTANCE_TO_STOP_PX: f32 = 0.5;
            const MIN_SPEED_TO_STOP: f32 = 0.5;

            loop {
                timer.await;
                timer = Timer::after(Duration::from_millis(16));

                let autoscroll_active_for_update = autoscroll_active_orig.clone();
                let scroll_handle_for_update = scroll_handle_orig.clone();

                let update_result = weak_entity.update(async_app_cx, move |view, model_cx| {
                    if !autoscroll_active_for_update.get() {
                        return false; // Stop task
                    }

                    // Access content_size and viewport_size through the view captured by the closure
                    let content_h = view.content_size.get().height;
                    let viewport_h = view.viewport_size.get().height;

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

                    if distance_to_target_abs_f32 < MIN_DISTANCE_TO_STOP_PX
                        && current_scroll_speed.abs() < MIN_SPEED_TO_STOP
                    {
                        scroll_handle_for_update.set_offset(Point {
                            x: px(0.0),
                            y: target_y_px,
                        });
                        autoscroll_active_for_update.set(false);
                        current_scroll_speed = 0.0;
                        return false; // Stop task
                    }

                    let force_spring_f32 = -SPRING_K * displacement_x_f32;
                    let force_damping_f32 = -DAMPING_C * current_scroll_speed;
                    let total_acceleration_f32 = force_spring_f32 + force_damping_f32;
                    current_scroll_speed += total_acceleration_f32;

                    let mut final_scroll_delta_f32 = current_scroll_speed;

                    if displacement_x_f32.abs() > f32::EPSILON {
                        let current_displacement_sign = displacement_x_f32.signum();
                        let planned_offset_y_f32 = current_offset_y_px.0 + final_scroll_delta_f32;
                        let planned_displacement_f32 = planned_offset_y_f32 - target_y_px.0;

                        if planned_displacement_f32.signum() != current_displacement_sign {
                            if distance_to_target_abs_f32 > MIN_DISTANCE_TO_STOP_PX {
                                final_scroll_delta_f32 = -displacement_x_f32;
                                current_scroll_speed = 0.0;
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

        *self.autoscroll_task.borrow_mut() = Some(task); // Use the renamed task field
    }

    /// Handle content size changes (detecting new content)
    fn handle_content_change(&self, new_height: f32, cx: &mut Context<Self>) {
        let old_height = self.last_content_height.get();

        // Check if at bottom BEFORE considering new height for auto-scroll decision logic
        // Use a tolerance, e.g., 50px, similar to the old config.bottom_threshold
        let at_bottom_before_update = self.is_at_bottom(px(50.0));
        self.was_at_bottom_before_update
            .set(at_bottom_before_update);
        // println!("ContentChange: was_at_bottom_before_update set to: {}", at_bottom_before_update);

        // Content grew (new content added)
        if new_height > old_height + 1.0 {
            // Ensure there's a noticeable growth
            self.last_content_height.set(new_height);

            // Decide if we need to autoscroll based on new logic
            if self.was_at_bottom_before_update.get() || self.autoscroll_active.get() {
                self.autoscroll_active.set(true);
                // println!("ContentChange: autoscroll_active set to true, starting task.");
                self.start_autoscroll_task(cx);
            } else {
                self.autoscroll_active.set(false);
                // println!("ContentChange: autoscroll_active set to false.");
            }
        }
    }

    /// Handle manual scrolling by user
    fn handle_manual_scroll(&self) {
        // Use a tolerance, e.g., 50px
        if self.is_at_bottom(px(50.0)) {
            // User scrolled back to bottom, re-enable auto-scroll possibility for next content add.
            // println!("ManualScroll: At bottom, autoscroll_active enabled.");
            self.autoscroll_active.set(true);
            // We don't start the task here; new content or handle_content_change will decide.
        } else {
            // User scrolled away from bottom, disable auto-scroll and stop any active animation.
            // println!("ManualScroll: Away from bottom, autoscroll_active disabled, task cancelled.");
            self.autoscroll_active.set(false);
            *self.autoscroll_task.borrow_mut() = None; // Cancel the animation task
        }
    }
}

impl Focusable for MessagesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MessagesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Get current messages to display
        let messages = {
            let lock = self.message_queue.lock().unwrap();
            lock.clone()
        };

        // Get the theme colors for user messages
        let user_accent = if cx.theme().is_dark() {
            rgb(0x6BD9A8) // Dark mode user accent
        } else {
            rgb(0x0A8A55) // Light mode user accent
        };

        // Clone handles for closures
        let content_size_rc = self.content_size.clone(); // Renamed for clarity in on_children_prepainted
        let viewport_size_rc = self.viewport_size.clone(); // Renamed for clarity
        let view_entity = cx.entity().clone(); // Clone entity for use in callbacks

        // Messages display area with scrollbar
        div()
            .on_children_prepainted({
                // ADDED: Listener for viewport_size
                let view_entity = view_entity.clone();
                let viewport_size_rc = viewport_size_rc.clone();
                move |bounds_vec: Vec<Bounds<Pixels>>, _window, app| {
                    if let Some(first_child_bounds) = bounds_vec.first() {
                        let new_viewport_size = first_child_bounds.size;
                        if viewport_size_rc.get() != new_viewport_size {
                            println!("view port size changed: {:?}", new_viewport_size);
                            viewport_size_rc.set(new_viewport_size);
                            // No cx.notify() needed here usually, as size changes often trigger repaint indirectly
                            // or are used in subsequent logic like is_at_bottom.
                            // view_entity.update(app, |view, cx| cx.notify()); // If explicit redraw needed
                        }
                    }
                }
            })
            .id("messages-container")
            .flex_1() // Take remaining space in the parent container
            .min_h_0() // Minimum height to ensure scrolling works
            .relative() // For absolute positioning of scrollbar
            .overflow_hidden() // ADDED: Crucial for stable viewport measurement
            .child(
                div()
                    .id("messages-scroll-container")
                    .size_full()
                    .overflow_hidden()
                    .child(
                        v_flex()
                            .on_children_prepainted({
                                // ADDED: Listener for content_size
                                let view_entity = view_entity.clone();
                                let content_size_rc = content_size_rc.clone();
                                move |bounds_vec: Vec<Bounds<Pixels>>, _window, app| {
                                    if let Some(first_child_bounds) = bounds_vec.first() {
                                        let new_content_size = first_child_bounds.size;
                                        if content_size_rc.get() != new_content_size {
                                            println!(
                                                "content size changed: {:?}",
                                                new_content_size
                                            );
                                            content_size_rc.set(new_content_size);
                                            // This is where content height changes are detected.
                                            // The original canvas called handle_content_change directly.
                                            // We need to replicate that behavior.
                                            let new_height = new_content_size.height.0;
                                            view_entity.update(app, |view, cx| {
                                                view.handle_content_change(new_height, cx);
                                                // cx.notify(); // handle_content_change calls notify if needed
                                            });
                                        }
                                    }
                                }
                            })
                            .id("messages")
                            .p_2()
                            .track_scroll(&self.scroll_handle)
                            .overflow_scroll()
                            .size_full()
                            .bg(cx.theme().card)
                            .gap_2()
                            .text_size(px(16.))
                            // Handle manual scroll events
                            .on_scroll_wheel(cx.listener(
                                move |view, _event: &gpui::ScrollWheelEvent, _window, _cx| {
                                    view.handle_manual_scroll();
                                },
                            ))
                            .children(messages.into_iter().map(|msg| {
                                // Create message container with appropriate styling based on role
                                let mut message_container = div().p_3().flex().flex_col().gap_2();

                                if msg.read(cx).is_user_message() {
                                    message_container = message_container
                                        .m_3()
                                        .bg(cx.theme().muted.opacity(0.3)) // Use theme muted color with opacity
                                        .rounded_md()
                                        .shadow_sm();
                                }

                                // Create message container with user badge if needed
                                let message_container = if msg.read(cx).is_user_message() {
                                    message_container.child(
                                        div().flex().flex_row().items_center().gap_2().children(
                                            vec![
                                                super::file_icons::render_icon_container(
                                                    &super::file_icons::get().get_type_icon(
                                                        super::file_icons::TOOL_USER_INPUT,
                                                    ),
                                                    16.0,
                                                    user_accent, // Use themed user accent color
                                                    "👤",
                                                )
                                                .into_any_element(),
                                                div()
                                                    .font_weight(gpui::FontWeight(600.0))
                                                    .text_color(user_accent) // Use themed user accent color
                                                    .child("You")
                                                    .into_any_element(),
                                            ],
                                        ),
                                    )
                                } else {
                                    message_container
                                };

                                // Render all block elements
                                let elements = msg.read(cx).elements();
                                let mut container_children = vec![];

                                // Add all existing blocks
                                for element in elements {
                                    container_children.push(element.into_any_element());
                                }

                                // Add loading indicator if waiting for content
                                if msg.read(cx).is_waiting_for_content() {
                                    container_children.push(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .p_2()
                                            .child(
                                                svg()
                                                    .size(px(16.))
                                                    .path(SharedString::from(
                                                        "icons/arrow_circle.svg",
                                                    ))
                                                    .text_color(cx.theme().info)
                                                    .with_animation(
                                                        "loading_indicator",
                                                        Animation::new(Duration::from_secs(2))
                                                            .repeat()
                                                            .with_easing(bounce(ease_in_out)),
                                                        |svg, delta| {
                                                            svg.with_transformation(
                                                                Transformation::rotate(percentage(
                                                                    delta,
                                                                )),
                                                            )
                                                        },
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_color(cx.theme().info)
                                                    .text_size(px(12.))
                                                    .child("Waiting for response..."),
                                            )
                                            .into_any_element(),
                                    );
                                }

                                message_container.children(container_children)
                            })),
                    )
                    // Add scrollbar
                    .child(
                        // Child 2: The manual scrollbar, absolutely positioned relative to outer_container
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(12.))
                            .child(Scrollbar::vertical(
                                cx.entity().entity_id(),
                                self.scrollbar_state.clone(),
                                self.scroll_handle.clone(),
                                self.content_size.get(),
                            )),
                    ),
            )
    }
}
