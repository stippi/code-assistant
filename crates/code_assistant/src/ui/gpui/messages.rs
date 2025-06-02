use super::elements::MessageContainer;
use gpui::{
    bounce, div, ease_in_out, percentage, prelude::*, px, rgb, svg, Animation, AnimationExt, App,
    Context, Entity, FocusHandle, Focusable, Pixels, Point, ScrollHandle, SharedString, Size, Task,
    Timer, Transformation, Window,
};
use gpui_component::{scroll::ScrollbarAxis, v_flex, ActiveTheme, StyledExt};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Configuration for the auto-scroll animation
#[derive(Clone)]
struct AutoScrollConfig {
    /// Base animation speed in pixels per second
    base_speed: f32,
    /// Maximum animation speed in pixels per second
    max_speed: f32,
    /// How much to accelerate when content is added frequently
    acceleration_factor: f32,
    /// Threshold distance from bottom to consider "at bottom"
    bottom_threshold: f32,
    /// Minimum time between scroll updates in milliseconds
    update_interval: u64,
}

impl Default for AutoScrollConfig {
    fn default() -> Self {
        Self {
            base_speed: 800.0, // pixels per second
            max_speed: 2400.0,
            acceleration_factor: 1.5,
            bottom_threshold: 50.0, // pixels from bottom
            update_interval: 16,    // ~60fps
        }
    }
}

/// State for auto-scroll animation
#[derive(Clone)]
struct AutoScrollState {
    /// Whether auto-scroll is currently enabled
    enabled: bool,
    /// Current target position (bottom of content)
    target_y: f32,
    /// Current animation speed
    current_speed: f32,
    /// Last time content was added
    last_content_added: Instant,
    /// Last time we updated the scroll position
    last_update: Instant,
    /// Whether we're currently animating
    animating: bool,
}

impl Default for AutoScrollState {
    fn default() -> Self {
        Self {
            enabled: true,
            target_y: 0.0,
            current_speed: AutoScrollConfig::default().base_speed,
            last_content_added: Instant::now(),
            last_update: Instant::now(),
            animating: false,
        }
    }
}

/// MessagesView - Component responsible for displaying the message history
pub struct MessagesView {
    message_queue: Arc<Mutex<Vec<Entity<MessageContainer>>>>,
    focus_handle: FocusHandle,

    // Auto-scroll functionality
    scroll_handle: ScrollHandle,
    scroll_state: Rc<RefCell<AutoScrollState>>,
    config: AutoScrollConfig,
    content_size: Rc<Cell<Size<Pixels>>>,
    viewport_size: Rc<Cell<Size<Pixels>>>,

    // Animation task
    scroll_task: Rc<RefCell<Option<Task<()>>>>,

    // Track message count to detect new messages
    last_message_count: Rc<Cell<usize>>,
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
            scroll_state: Rc::new(RefCell::new(AutoScrollState::default())),
            config: AutoScrollConfig::default(),
            content_size: Rc::new(Cell::new(Size::default())),
            viewport_size: Rc::new(Cell::new(Size::default())),
            scroll_task: Rc::new(RefCell::new(None)),
            last_message_count: Rc::new(Cell::new(0)),
        }
    }

    /// Check if we're currently at the bottom of the scroll area
    fn is_at_bottom(&self) -> bool {
        let current_offset = self.scroll_handle.offset();
        let content_height = self.content_size.get().height.0;
        let viewport_height = self.viewport_size.get().height.0;

        if content_height <= viewport_height {
            return true; // No scrolling needed
        }

        let max_scroll = content_height - viewport_height;
        let distance_from_bottom = max_scroll - (-current_offset.y.0);

        distance_from_bottom <= self.config.bottom_threshold
    }

    /// Start or update the auto-scroll animation
    fn start_auto_scroll_animation(&self, cx: &mut Context<Self>) {
        let scroll_handle = self.scroll_handle.clone();
        let scroll_state = self.scroll_state.clone();
        let config = self.config.clone();
        let content_size = self.content_size.clone();
        let viewport_size = self.viewport_size.clone();

        // Cancel any existing animation
        *self.scroll_task.borrow_mut() = None;

        let task = cx.spawn(async move |weak_entity, cx| {
            let mut timer = Timer::after(Duration::from_millis(config.update_interval));

            loop {
                timer.await;
                timer = Timer::after(Duration::from_millis(config.update_interval));

                let should_continue = weak_entity
                    .update(cx, |_view, cx| {
                        let mut state = scroll_state.borrow_mut();

                        if !state.enabled {
                            state.animating = false;
                            return false;
                        }

                        let current_offset = scroll_handle.offset();
                        let target_y = {
                            let content_height = content_size.get().height.0;
                            let viewport_height = viewport_size.get().height.0;

                            if content_height <= viewport_height {
                                0.0
                            } else {
                                -(content_height - viewport_height)
                            }
                        };

                        // Update target if content changed
                        if (target_y - state.target_y).abs() > 1.0 {
                            state.target_y = target_y;
                            state.last_content_added = Instant::now();

                            // Increase speed when content is added frequently
                            let time_since_last_add =
                                state.last_content_added.duration_since(state.last_update);
                            if time_since_last_add < Duration::from_millis(500) {
                                state.current_speed = (state.current_speed
                                    * config.acceleration_factor)
                                    .min(config.max_speed);
                            }
                        }

                        let current_y = current_offset.y.0;
                        let distance_to_target = target_y - current_y;

                        // Check if we've reached the target
                        if distance_to_target.abs() < 1.0 {
                            state.animating = false;
                            return false;
                        }

                        // Calculate smooth movement
                        let dt = state.last_update.elapsed().as_secs_f32();
                        let max_move = state.current_speed * dt;

                        let move_distance = if distance_to_target.abs() < max_move {
                            distance_to_target
                        } else {
                            distance_to_target.signum() * max_move
                        };

                        let new_y = current_y + move_distance;
                        scroll_handle.set_offset(Point::new(current_offset.x, px(new_y)));

                        state.last_update = Instant::now();
                        state.animating = true;

                        // Gradually slow down if no new content
                        if state.last_content_added.elapsed() > Duration::from_millis(1000) {
                            state.current_speed =
                                (state.current_speed * 0.98).max(config.base_speed);
                        }

                        cx.notify();
                        true
                    })
                    .unwrap_or(false);

                if !should_continue {
                    break;
                }
            }
        });

        *self.scroll_task.borrow_mut() = Some(task);
        self.scroll_state.borrow_mut().animating = true;
    }

    /// Handle new messages being added
    fn handle_new_messages(&self, new_count: usize, cx: &mut Context<Self>) {
        let old_count = self.last_message_count.get();

        if new_count > old_count {
            self.last_message_count.set(new_count);

            // Only auto-scroll if we were already at the bottom
            if self.is_at_bottom() {
                self.scroll_state.borrow_mut().enabled = true;
                self.start_auto_scroll_animation(cx);
            }
        }
    }

    /// Handle manual scrolling by user
    fn handle_manual_scroll(&self) {
        if self.is_at_bottom() {
            // User scrolled back to bottom, re-enable auto-scroll
            self.scroll_state.borrow_mut().enabled = true;
        } else {
            // User scrolled away from bottom, disable auto-scroll
            let mut state = self.scroll_state.borrow_mut();
            state.enabled = false;
            state.animating = false;
            // Cancel animation task
            drop(state);
            *self.scroll_task.borrow_mut() = None;
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

        // Check for new messages
        self.handle_new_messages(messages.len(), cx);

        // Get the theme colors for user messages
        let user_accent = if cx.theme().is_dark() {
            rgb(0x6BD9A8) // Dark mode user accent
        } else {
            rgb(0x0A8A55) // Light mode user accent
        };

        // Clone handles for closures
        let content_size = self.content_size.clone();
        let viewport_size = self.viewport_size.clone();

        // Messages display area with scrollbar
        div()
            .id("messages-container")
            .flex_1() // Take remaining space in the parent container
            .min_h_0() // Minimum height to ensure scrolling works
            .relative() // For absolute positioning of scrollbar
            .child(
                v_flex()
                    .id("messages")
                    .flex_1()
                    .p_2()
                    .track_scroll(&self.scroll_handle)
                    .scrollable(cx.entity().entity_id(), ScrollbarAxis::Vertical)
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
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .children(vec![
                                        super::file_icons::render_icon_container(
                                            &super::file_icons::get()
                                                .get_type_icon(super::file_icons::TOOL_USER_INPUT),
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
                                    ]),
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
                                            .size(px(18.))
                                            .path(SharedString::from("icons/arrow_circle.svg"))
                                            .text_color(cx.theme().info)
                                            .with_animation(
                                                "loading_indicator",
                                                Animation::new(Duration::from_secs(2))
                                                    .repeat()
                                                    .with_easing(bounce(ease_in_out)),
                                                |svg, delta| {
                                                    svg.with_transformation(Transformation::rotate(
                                                        percentage(delta),
                                                    ))
                                                },
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_color(cx.theme().info)
                                            .text_size(px(14.))
                                            .child("Waiting for response..."),
                                    )
                                    .into_any_element(),
                            );
                        }

                        message_container.children(container_children)
                    }))
                    // Add a canvas to track content size changes
                    .child({
                        gpui::canvas(
                            move |bounds, _window, _cx| {
                                //println!("update content size: {:?}", bounds.size);
                                content_size.set(bounds.size);
                            },
                            move |_bounds, _window, _element_state, _cx| {},
                        )
                        .absolute()
                        .size_full()
                    }),
            )
            // Add a canvas to track viewport size
            .child({
                let viewport_size = viewport_size.clone();
                gpui::canvas(
                    move |bounds, _window, _cx| {
                        //println!("update view port size: {:?}", bounds.size);
                        viewport_size.set(bounds.size);
                    },
                    move |_bounds, _window, _element_state, _cx| {},
                )
                .absolute()
                .size_full()
            })
    }
}
