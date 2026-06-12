//! Floating status popover rendered over the messages area.
//!
//! Shows transient error messages or status notifications.

use gpui::{div, px, rems, rgba, svg, AnyElement, SharedString, Styled};
use gpui::{prelude::*, Context};
use gpui_component::ActiveTheme;

use crate::{Gpui, UiEventSender};
use code_assistant_core::ui::ui_events::UiEvent;

use super::MainScreen;

/// Render the floating status popover if needed (currently: errors and transient status).
/// Returns an empty Vec when nothing should be shown.
pub(super) fn render_status_popover(
    _main_screen: &MainScreen,
    cx: &mut Context<MainScreen>,
) -> Vec<AnyElement> {
    // Get current error from global Gpui
    let current_error = if let Some(gpui) = cx.try_global::<Gpui>() {
        gpui.get_current_error()
    } else {
        None
    };

    // Check for error first (higher priority than activity states)
    if let Some(error_message) = current_error {
        let (bg_color, border_color, text_color) = if cx.theme().is_dark() {
            (
                rgba(0x7F1D1D80), // Dark red background with transparency
                rgba(0xEF4444FF), // Red border
                rgba(0xFCA5A5FF), // Light red text
            )
        } else {
            (
                rgba(0xFEF2F2FF), // Light red background
                rgba(0xF87171FF), // Red border
                rgba(0xDC2626FF), // Dark red text
            )
        };

        // Return the error popover positioned at bottom of scroll area
        return vec![div()
            .absolute()
            .bottom_2() // Small gap from the bottom of the scroll area
            .left(px(0.))
            .right(px(0.))
            .flex()
            .justify_center() // Center the content horizontally
            .child(
                div()
                    .px_4()
                    .py_2()
                    .bg(bg_color)
                    .border_1()
                    .border_color(border_color)
                    .rounded_lg()
                    .shadow_lg()
                    .overflow_hidden()
                    .flex()
                    .items_start() // Align items to top for multi-line text
                    .gap_2()
                    .max_w(px(600.)) // Limit width for long error messages
                    .min_w(px(200.)) // Ensure minimum width
                    .child(
                        div()
                            .flex_none()
                            .mt(px(1.)) // Slight top margin to align with first line of text
                            .child(
                                svg()
                                    .size(px(14.))
                                    .path(SharedString::from("icons/circle_stop.svg"))
                                    .text_color(text_color),
                            ),
                    )
                    .child(
                        div()
                            .text_color(text_color)
                            .text_size(rems(0.6875))
                            .font_weight(gpui::FontWeight(500.0))
                            .flex_grow()
                            .flex_shrink()
                            .min_w_0() // Allow shrinking below content size for text wrapping
                            .overflow_hidden() // Prevent text from overflowing
                            .whitespace_normal() // Enable text wrapping
                            .line_height(rems(0.875)) // Set line height for better readability
                            .child(error_message),
                    )
                    .child(
                        // Add a close button
                        div()
                            .id("error-close-btn")
                            .flex_none()
                            .size(px(20.))
                            .rounded_sm()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| s.bg(cx.theme().muted.opacity(0.3)))
                            .child(
                                svg()
                                    .size(px(12.))
                                    .path(SharedString::from("icons/close.svg"))
                                    .text_color(text_color),
                            )
                            .on_click(|_, _, cx| {
                                if let Some(sender) = cx.try_global::<UiEventSender>() {
                                    let _ = sender.0.try_send(UiEvent::ClearError);
                                }
                            }),
                    ),
            )
            .into_any_element()];
    }

    // Transient status notification (lower priority than errors)
    let transient_status = if let Some(gpui) = cx.try_global::<Gpui>() {
        gpui.get_transient_status()
    } else {
        None
    };

    if let Some(status_message) = transient_status {
        let (bg_color, border_color, text_color, icon_color) = if cx.theme().is_dark() {
            (
                rgba(0x78350F80), // Dark amber background with transparency
                rgba(0xF59E0BFF), // Amber border
                rgba(0xFDE68AFF), // Light amber text
                rgba(0xFBBF24FF), // Amber icon
            )
        } else {
            (
                rgba(0xFFFBEBFF), // Light amber background
                rgba(0xF59E0BFF), // Amber border
                rgba(0x92400EFF), // Dark amber text
                rgba(0xD97706FF), // Amber icon
            )
        };

        return vec![div()
            .absolute()
            .bottom_2()
            .left(px(0.))
            .right(px(0.))
            .flex()
            .justify_center()
            .child(
                div()
                    .px_4()
                    .py_2()
                    .bg(bg_color)
                    .border_1()
                    .border_color(border_color)
                    .rounded_lg()
                    .shadow_lg()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .gap_2()
                    .max_w(px(600.))
                    .min_w(px(200.))
                    .child(
                        div().flex_none().child(
                            svg()
                                .size(px(14.))
                                .path(SharedString::from("icons/arrow_circle.svg"))
                                .text_color(icon_color),
                        ),
                    )
                    .child(
                        div()
                            .text_color(text_color)
                            .text_size(rems(0.6875))
                            .font_weight(gpui::FontWeight(500.0))
                            .flex_grow()
                            .flex_shrink()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_normal()
                            .line_height(rems(0.875))
                            .child(status_message),
                    ),
            )
            .into_any_element()];
    }

    // Activity states (WaitingForResponse, RateLimited) are now shown
    // inline at the bottom of the messages list — no floating popover.

    vec![] // No popover to show
}
