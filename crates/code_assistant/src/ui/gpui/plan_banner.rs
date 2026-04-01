use super::file_icons;
use crate::types::{PlanItemStatus, PlanState};
use gpui::prelude::*;
use gpui::{
    div, percentage, px, rems, Animation, AnimationExt, Bounds, ClickEvent, Context, EventEmitter,
    Pixels, Render, SharedString, Task, Timer, Transformation, Window,
};
use gpui_component::{ActiveTheme, StyledExt};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Animation helpers (same approach as tool blocks in elements.rs)
// ---------------------------------------------------------------------------

const ANIMATION_DURATION_MS: f32 = 250.0;
const ANIMATION_FRAME_MS: u64 = 8; // ~120 FPS

#[derive(Clone, Debug, PartialEq)]
enum AnimationState {
    Idle,
    Animating {
        height_scale: f32,
        target: f32, // 0.0 = collapsing, 1.0 = expanding
        start_time: Instant,
    },
}

// ---------------------------------------------------------------------------
// PlanBanner
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub enum PlanBannerEvent {
    Toggle { collapsed: bool },
}

pub struct PlanBanner {
    plan: Option<PlanState>,
    collapsed: bool,
    // Animation
    animation_state: AnimationState,
    content_height: Rc<Cell<Pixels>>,
    animation_task: Option<Task<()>>,
}

impl Default for PlanBanner {
    fn default() -> Self {
        Self {
            plan: None,
            collapsed: false,
            animation_state: AnimationState::Idle,
            content_height: Rc::new(Cell::new(px(0.0))),
            animation_task: None,
        }
    }
}

impl PlanBanner {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self::default()
    }

    pub fn set_plan(&mut self, plan: Option<PlanState>, collapsed: bool, cx: &mut Context<Self>) {
        let plan_changed = self.plan != plan;
        self.plan = plan;

        // Only reset animation when the collapsed state is forced externally
        // (e.g. session switch) — NOT during a user-initiated toggle animation.
        let is_animating = !matches!(self.animation_state, AnimationState::Idle);
        if !is_animating && self.collapsed != collapsed {
            self.collapsed = collapsed;
        }

        if plan_changed {
            cx.notify();
        }
    }

    fn on_toggle(&mut self, _event: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let should_expand = self.collapsed; // if currently collapsed, we expand
        self.collapsed = !self.collapsed;
        cx.emit(PlanBannerEvent::Toggle {
            collapsed: self.collapsed,
        });
        self.start_animation(should_expand, cx);
        cx.notify();
    }

    fn start_animation(&mut self, should_expand: bool, cx: &mut Context<Self>) {
        let target = if should_expand { 1.0 } else { 0.0 };
        let now = Instant::now();

        match &self.animation_state {
            AnimationState::Animating {
                height_scale,
                target: current_target,
                ..
            } if *current_target != target => {
                // Reverse: keep current scale, adjust start for smooth transition
                let current_progress = if target == 1.0 {
                    *height_scale
                } else {
                    1.0 - *height_scale
                };
                let adjusted_start =
                    now - Duration::from_millis((current_progress * ANIMATION_DURATION_MS) as u64);
                self.animation_state = AnimationState::Animating {
                    height_scale: *height_scale,
                    target,
                    start_time: adjusted_start,
                };
            }
            _ => {
                let initial = if should_expand { 0.0 } else { 1.0 };
                self.animation_state = AnimationState::Animating {
                    height_scale: initial,
                    target,
                    start_time: now,
                };
            }
        }

        if self.animation_task.is_none() {
            self.start_animation_task(cx);
        }
    }

    fn start_animation_task(&mut self, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |weak_entity, async_cx| {
            let mut timer = Timer::after(Duration::from_millis(ANIMATION_FRAME_MS));
            loop {
                timer.await;
                timer = Timer::after(Duration::from_millis(ANIMATION_FRAME_MS));

                let should_continue = weak_entity.update(async_cx, |view, cx| {
                    view.update_animation();
                    match &view.animation_state {
                        AnimationState::Idle => false,
                        _ => {
                            cx.notify();
                            true
                        }
                    }
                });

                if let Ok(should_continue) = should_continue {
                    if !should_continue {
                        let _ = weak_entity.update(async_cx, |view, _cx| {
                            view.animation_task = None;
                        });
                        break;
                    }
                } else {
                    break;
                }
            }
        });
        self.animation_task = Some(task);
    }

    fn update_animation(&mut self) {
        match &mut self.animation_state {
            AnimationState::Animating {
                height_scale,
                target,
                start_time,
            } => {
                let elapsed = start_time.elapsed().as_millis() as f32;
                let progress = (elapsed / ANIMATION_DURATION_MS).min(1.0);
                // ease-out cubic
                let eased = 1.0 - (1.0 - progress).powi(3);

                *height_scale = if *target == 1.0 { eased } else { 1.0 - eased };

                if progress >= 1.0 {
                    *height_scale = *target;
                    self.animation_state = AnimationState::Idle;
                }
            }
            AnimationState::Idle => {}
        }
    }

    /// Current animation scale: 0.0 = items fully hidden, 1.0 = items fully visible
    fn animation_scale(&self) -> f32 {
        match &self.animation_state {
            AnimationState::Animating { height_scale, .. } => *height_scale,
            AnimationState::Idle => {
                if self.collapsed {
                    0.0
                } else {
                    1.0
                }
            }
        }
    }
}

impl EventEmitter<PlanBannerEvent> for PlanBanner {}

impl Render for PlanBanner {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(plan) = self.plan.as_ref() else {
            return div().into_any_element();
        };

        if plan.entries.is_empty() {
            return div().into_any_element();
        }

        let total = plan.entries.len();
        let completed = plan
            .entries
            .iter()
            .filter(|e| e.status == PlanItemStatus::Completed)
            .count();
        let all_done = completed == total;
        let scale = self.animation_scale();

        let in_progress_item = plan
            .entries
            .iter()
            .find(|e| e.status == PlanItemStatus::InProgress);

        // Chevron: use target state (collapsed) not animation state
        let (chevron_icon, chevron_fallback) = if self.collapsed {
            (file_icons::get().get_type_icon(file_icons::CHEVRON_UP), "▲")
        } else {
            (
                file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN),
                "▼",
            )
        };

        let toggle = cx.listener(Self::on_toggle);

        // -- Status text --
        let status_text = if all_done {
            "All Done".to_string()
        } else if completed > 0 {
            format!("{completed}/{total}")
        } else {
            format!("{total} {}", if total == 1 { "item" } else { "items" })
        };

        // -- Header --
        // When collapsed (target) AND not mid-animation-expand, show active item inline
        let show_inline_active = self.collapsed && scale < 0.5;

        let header = if show_inline_active {
            if let Some(active) = in_progress_item {
                let truncated = truncate_text(&normalize_single_line(&active.content), 60);
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(px(6.))
                            .child(render_chevron(&chevron_icon, chevron_fallback, cx))
                            .child(render_plan_label(cx))
                            .child(
                                div()
                                    .text_size(rems(0.6875))
                                    .text_color(cx.theme().muted_foreground)
                                    .child("•"),
                            )
                            .child(
                                gpui::svg()
                                    .flex_none()
                                    .size(px(12.))
                                    .path("icons/arrow_circle.svg")
                                    .text_color(cx.theme().info)
                                    .with_animation(
                                        "plan-active-spin-header",
                                        Animation::new(Duration::from_secs(2)).repeat(),
                                        |svg, delta| {
                                            svg.with_transformation(Transformation::rotate(
                                                percentage(delta),
                                            ))
                                        },
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_size(rems(0.6875))
                                    .text_color(cx.theme().info)
                                    .child(SharedString::from(truncated)),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(rems(0.6875))
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(format!("{completed}/{total}"))),
                    )
            } else {
                // Default header (all done or no active item)
                render_default_header(&chevron_icon, chevron_fallback, &status_text, cx)
            }
        } else {
            render_default_header(&chevron_icon, chevron_fallback, &status_text, cx)
        };

        let header_row = div()
            .id("plan-toggle-btn")
            .cursor_pointer()
            .rounded_md()
            .hover(|s| s.bg(cx.theme().muted.opacity(0.3)))
            .px_1()
            .py(px(2.))
            .child(header)
            .on_click(toggle);

        let mut container = div()
            .id("session-plan")
            .flex()
            .flex_col()
            .flex_none()
            .bg(cx.theme().background)
            .border_t_1()
            .border_color(cx.theme().border)
            .px_3()
            .py(px(6.))
            .gap(px(2.))
            .child(header_row);

        // Show items if expanded OR during animation (scale > 0)
        if scale > 0.0 {
            let items_container = render_plan_items(plan, cx);

            // Wrap in animated height container
            let content_height = self.content_height.clone();
            let height_for_render = content_height.clone();

            let animated_wrapper = div()
                .overflow_hidden()
                .when(scale < 1.0, move |d| {
                    let h = height_for_render.get();
                    if h > px(0.0) {
                        d.h(h * scale)
                    } else {
                        d.h(px(0.0))
                    }
                })
                .on_children_prepainted({
                    move |bounds_vec: Vec<Bounds<Pixels>>, _window, _app| {
                        if let Some(first) = bounds_vec.first() {
                            let new_h = first.size.height;
                            if content_height.get() != new_h {
                                content_height.set(new_h);
                            }
                        }
                    }
                })
                .child(items_container);

            container = container.child(animated_wrapper);
        }

        container.into_any_element()
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers (free functions to keep Render::render concise)
// ---------------------------------------------------------------------------

fn render_chevron(
    icon: &Option<SharedString>,
    fallback: &str,
    cx: &mut Context<PlanBanner>,
) -> gpui::Div {
    div()
        .size(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .child(file_icons::render_icon(
            icon,
            12.0,
            cx.theme().muted_foreground,
            fallback,
        ))
}

fn render_plan_label(cx: &mut Context<PlanBanner>) -> gpui::Div {
    div()
        .text_size(rems(0.75))
        .font_medium()
        .text_color(cx.theme().foreground)
        .child("Plan")
}

fn render_default_header(
    chevron_icon: &Option<SharedString>,
    chevron_fallback: &str,
    status_text: &str,
    cx: &mut Context<PlanBanner>,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(6.))
                .child(render_chevron(chevron_icon, chevron_fallback, cx))
                .child(render_plan_label(cx)),
        )
        .child(
            div()
                .flex_none()
                .text_size(rems(0.6875))
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(status_text.to_string())),
        )
}

fn render_plan_items(plan: &PlanState, cx: &mut Context<PlanBanner>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(1.))
        .children(plan.entries.iter().enumerate().map(|(idx, entry)| {
            let is_in_progress = entry.status == PlanItemStatus::InProgress;
            let is_completed = entry.status == PlanItemStatus::Completed;

            let (icon_color, text_color) = if is_completed {
                (cx.theme().success, cx.theme().muted_foreground)
            } else if is_in_progress {
                (cx.theme().info, cx.theme().foreground)
            } else {
                (
                    cx.theme().muted_foreground.opacity(0.5),
                    cx.theme().foreground,
                )
            };

            div()
                .flex()
                .items_start()
                .gap(px(6.))
                .py(px(3.))
                .px_1()
                .rounded_md()
                .when(is_in_progress, |el| el.bg(cx.theme().info.opacity(0.06)))
                .child(
                    div()
                        .flex_none()
                        .size(px(16.))
                        .mt(px(1.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(is_completed, |el| {
                            el.child(
                                gpui::svg()
                                    .size(px(14.))
                                    .path("icons/check_circle.svg")
                                    .text_color(icon_color),
                            )
                        })
                        .when(is_in_progress, |el| {
                            el.child(
                                gpui::svg()
                                    .size(px(14.))
                                    .path("icons/arrow_circle.svg")
                                    .text_color(icon_color)
                                    .with_animation(
                                        SharedString::from(format!("plan-spin-{idx}")),
                                        Animation::new(Duration::from_secs(2)).repeat(),
                                        |svg, delta| {
                                            svg.with_transformation(Transformation::rotate(
                                                percentage(delta),
                                            ))
                                        },
                                    ),
                            )
                        })
                        .when(!is_completed && !is_in_progress, |el| {
                            el.child(
                                div()
                                    .size(px(12.))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(icon_color),
                            )
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(rems(0.75))
                        .line_height(rems(1.125))
                        .text_color(text_color)
                        .child(SharedString::from(normalize_single_line(&entry.content))),
                )
        }))
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

fn normalize_single_line(content: &str) -> String {
    content
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_text(text: &str, max_len: usize) -> String {
    if text.chars().count() <= max_len {
        text.to_string()
    } else {
        let mut truncated = text
            .chars()
            .take(max_len.saturating_sub(1))
            .collect::<String>();
        truncated.push('…');
        truncated
    }
}
