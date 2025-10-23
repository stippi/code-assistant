use super::file_icons;
use crate::types::{PlanItemPriority, PlanItemStatus, PlanState};
use gpui::prelude::*;
use gpui::{div, px, Context, EventEmitter, MouseButton, MouseUpEvent, Render, Window};
use gpui_component::{text::TextView, ActiveTheme};

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

    fn on_toggle(&mut self, _event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.collapsed = !self.collapsed;
        cx.emit(PlanBannerEvent::Toggle {
            collapsed: self.collapsed,
        });
        cx.notify();
    }
}

impl EventEmitter<PlanBannerEvent> for PlanBanner {}

impl Render for PlanBanner {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(plan) = self.plan.as_ref() else {
            return div().into_any_element();
        };

        if plan.entries.is_empty() {
            return div().into_any_element();
        }

        let (summary_text, highlight_summary) = collapsed_plan_summary(plan);
        let item_count = plan.entries.len();
        let item_label = if item_count == 1 { "item" } else { "items" };

        let (chevron_icon, chevron_fallback) = if self.collapsed {
            (file_icons::get().get_type_icon(file_icons::CHEVRON_UP), "▲")
        } else {
            (
                file_icons::get().get_type_icon(file_icons::CHEVRON_DOWN),
                "▼",
            )
        };

        let toggle = cx.listener(Self::on_toggle);

        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(11.))
                    .text_color(cx.theme().muted_foreground)
                    .child("Plan")
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground.opacity(0.75))
                            .child(format!("• {item_count} {item_label}")),
                    ),
            )
            .child(
                div()
                    .size(px(20.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(cx.theme().muted))
                    .child(file_icons::render_icon(
                        &chevron_icon,
                        14.0,
                        cx.theme().muted_foreground,
                        chevron_fallback,
                    ))
                    .on_mouse_up(MouseButton::Left, toggle),
            );

        let mut container = div()
            .id("session-plan")
            .flex()
            .flex_col()
            .flex_none()
            .bg(cx.theme().background)
            .border_t_1()
            .border_color(cx.theme().border)
            .px_4()
            .py_3()
            .gap_2()
            .text_size(px(11.))
            .line_height(px(15.))
            .child(header);

        if self.collapsed {
            let summary_color = if highlight_summary {
                cx.theme().info
            } else {
                cx.theme().muted_foreground
            };
            container = container.child(div().text_color(summary_color).child(summary_text));
        } else {
            let markdown = build_plan_markdown(plan);
            if !markdown.is_empty() {
                container = container.child(div().text_color(cx.theme().foreground).child(
                    TextView::markdown("session-plan-markdown", markdown, window, cx).selectable(),
                ));
            }
        }

        container.into_any_element()
    }
}

fn build_plan_markdown(plan_state: &PlanState) -> String {
    plan_state
        .entries
        .iter()
        .map(|entry| {
            let checkbox = match entry.status {
                PlanItemStatus::Pending | PlanItemStatus::InProgress => "[ ]",
                PlanItemStatus::Completed => "[x]",
            };

            let mut line = format!("- {} {}", checkbox, escape_markdown(&entry.content));

            if entry.status == PlanItemStatus::InProgress {
                line.push_str(" _(in progress)_");
            }

            match entry.priority {
                PlanItemPriority::High => line.push_str(" **(high priority)**"),
                PlanItemPriority::Low => line.push_str(" _(low priority)_"),
                PlanItemPriority::Medium => {}
            }

            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn collapsed_plan_summary(plan_state: &PlanState) -> (String, bool) {
    if let Some(in_progress) = plan_state
        .entries
        .iter()
        .find(|entry| entry.status == PlanItemStatus::InProgress)
    {
        let normalized = normalize_single_line(&in_progress.content);
        let truncated = truncate_text(&normalized, 80);
        (format!("In progress • {truncated}"), true)
    } else {
        let count = plan_state.entries.len();
        let label = if count == 1 {
            "plan item"
        } else {
            "plan items"
        };
        (format!("{count} {label}"), false)
    }
}

fn escape_markdown(content: &str) -> String {
    let collapsed = normalize_single_line(content);
    collapsed
        .replace('&', "&amp;")
        .replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('(', "\\(")
        .replace(')', "\\)")
        .replace('<', "\\<")
        .replace('#', "\\#")
        .replace('+', "\\+")
        .replace('!', "\\!")
        .replace('|', "\\|")
        .replace('>', "\\>")
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
