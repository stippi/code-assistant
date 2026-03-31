use super::file_icons;
use crate::types::{PlanItemStatus, PlanState};
use gpui::prelude::*;
use gpui::{
    div, percentage, px, Animation, AnimationExt, ClickEvent, Context, EventEmitter, Render,
    SharedString, Transformation, Window,
};
use gpui_component::{ActiveTheme, StyledExt};
use std::time::Duration;

#[derive(Clone)]
pub enum PlanBannerEvent {
    Toggle { collapsed: bool },
}

#[derive(Default)]
pub struct PlanBanner {
    plan: Option<PlanState>,
    collapsed: bool,
}

impl PlanBanner {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self::default()
    }

    pub fn set_plan(&mut self, plan: Option<PlanState>, collapsed: bool, cx: &mut Context<Self>) {
        self.plan = plan;
        self.collapsed = collapsed;
        cx.notify();
    }

    fn on_toggle(&mut self, _event: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.collapsed = !self.collapsed;
        cx.emit(PlanBannerEvent::Toggle {
            collapsed: self.collapsed,
        });
        cx.notify();
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

        let in_progress_item = plan
            .entries
            .iter()
            .find(|e| e.status == PlanItemStatus::InProgress);

        // Chevron icon: ▼ when expanded (content visible below), ▲ when collapsed
        let (chevron_icon, chevron_fallback) = if self.collapsed {
            (file_icons::get().get_type_icon(file_icons::CHEVRON_UP), "▲")
        } else {
            (
                file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN),
                "▼",
            )
        };

        let toggle = cx.listener(Self::on_toggle);

        // -- Status text for the right side of the header --
        let status_text = if all_done {
            "All Done".to_string()
        } else if completed > 0 {
            format!("{completed}/{total}")
        } else {
            format!("{total} {}", if total == 1 { "item" } else { "items" })
        };

        // -- Header row --
        let mut header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                // Left side: chevron + "Plan" + optional in-progress text when collapsed
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(file_icons::render_icon(
                                &chevron_icon,
                                12.0,
                                cx.theme().muted_foreground,
                                chevron_fallback,
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_medium()
                            .text_color(cx.theme().foreground)
                            .child("Plan"),
                    ),
            )
            .child(
                // Right side: status text
                div()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(status_text)),
            );

        // When collapsed, show the active item inline in the header
        if self.collapsed {
            if let Some(active) = in_progress_item {
                let truncated = truncate_text(&normalize_single_line(&active.content), 60);
                header = div()
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
                            .child(
                                div()
                                    .size(px(16.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(file_icons::render_icon(
                                        &chevron_icon,
                                        12.0,
                                        cx.theme().muted_foreground,
                                        chevron_fallback,
                                    )),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_medium()
                                    .text_color(cx.theme().foreground)
                                    .child("Plan"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child("•"),
                            )
                            .child(
                                // Spinning icon for the active item
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
                                    .text_size(px(11.))
                                    .text_color(cx.theme().info)
                                    .child(SharedString::from(truncated)),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(format!("{completed}/{total}"))),
                    );
            } else if all_done {
                // All done — no need to show anything extra, header already says "All Done"
            }
        }

        // Wrap header in clickable container
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

        // Expanded: show plan items directly (no markdown)
        if !self.collapsed {
            let items_container =
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
                            // Icon: check_circle for done, spinning arrow for in-progress, empty circle for pending
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(16.))
                                    .mt(px(1.)) // fine-tune vertical alignment with text
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
                                                        svg.with_transformation(
                                                            Transformation::rotate(percentage(
                                                                delta,
                                                            )),
                                                        )
                                                    },
                                                ),
                                        )
                                    })
                                    .when(!is_completed && !is_in_progress, |el| {
                                        el.child(
                                            // Empty circle for pending
                                            div()
                                                .size(px(12.))
                                                .rounded_full()
                                                .border_1()
                                                .border_color(icon_color),
                                        )
                                    }),
                            )
                            // Text
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(12.))
                                    .line_height(px(18.))
                                    .text_color(text_color)
                                    .child(SharedString::from(normalize_single_line(
                                        &entry.content,
                                    ))),
                            )
                    }));

            container = container.child(items_container);
        }

        container.into_any_element()
    }
}

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
