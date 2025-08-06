use super::file_icons;
use super::image;
use crate::persistence::DraftAttachment;
use gpui::{
    div, img, prelude::*, px, Context, FocusHandle, Focusable, ImageSource, InteractiveElement,
    MouseButton, ObjectFit, SharedString, Window,
};
use gpui_component::ActiveTheme;

/// Maximum size for attachment thumbnails
const ATTACHMENT_THUMBNAIL_SIZE: f32 = 80.0;

/// Individual attachment preview component with hover state
pub struct AttachmentView {
    attachment: DraftAttachment,
    index: usize,
    is_hovered: bool,
    focus_handle: FocusHandle,
}

impl AttachmentView {
    pub fn new(attachment: DraftAttachment, index: usize, cx: &mut Context<Self>) -> Self {
        Self {
            attachment,
            index,
            is_hovered: false,
            focus_handle: cx.focus_handle(),
        }
    }

    fn on_hover(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if *hovered != self.is_hovered {
            self.is_hovered = *hovered;
            cx.notify();
        }
    }

    fn render_content(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        match &self.attachment {
            DraftAttachment::Image { mime_type, content } => {
                // Try to parse and render the actual image
                let parsed_image = image::parse_base64_image(mime_type, content);

                if let Some(image) = parsed_image {
                    // Render actual image thumbnail
                    img(ImageSource::Image(image))
                        .size_full()
                        .object_fit(ObjectFit::Cover) // Cover for thumbnails to fill the square
                        .into_any_element()
                } else {
                    // Fallback to text if parsing failed
                    let display_text = mime_type.split('/').next_back().unwrap_or("image").to_string();
                    div()
                        .bg(cx.theme().muted)
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("⚠️ {display_text}")),
                        )
                        .into_any_element()
                }
            }
            DraftAttachment::Text { .. } => div()
                .bg(cx.theme().muted)
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("text"),
                )
                .into_any_element(),
            DraftAttachment::File { filename, .. } => div()
                .bg(cx.theme().muted)
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("📄"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .text_ellipsis()
                        .w_full()
                        .text_center()
                        .child(filename.clone()),
                )
                .into_any_element(),
        }
    }
}

impl Focusable for AttachmentView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AttachmentView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let index = self.index;

        div()
            .id(SharedString::from(format!("attachment-{index}")))
            .relative()
            .w(px(ATTACHMENT_THUMBNAIL_SIZE))
            .h(px(ATTACHMENT_THUMBNAIL_SIZE))
            .rounded_md()
            .bg(cx.theme().card)
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .shadow_sm()
            .flex()
            .items_center()
            .justify_center()
            .on_hover(cx.listener(Self::on_hover))
            .child(self.render_content(cx))
            .when(self.is_hovered, |container| {
                container.child(
                    // Remove button - only visible on hover
                    div()
                        .absolute()
                        .top(px(4.))
                        .right(px(4.))
                        .size(px(20.))
                        .rounded_sm()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(cx.theme().danger.opacity(0.1)))
                        .child(file_icons::render_icon(
                            &file_icons::get().get_type_icon("trash"),
                            12.0,
                            cx.theme().danger,
                            "🗑",
                        ))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |_view, _event, _window, cx| {
                                // Emit a custom event that the parent can listen to
                                cx.emit(AttachmentEvent::Remove(index));
                            }),
                        ),
                )
            })
    }
}

/// Events that can be emitted by AttachmentView
#[derive(Clone, Debug)]
pub enum AttachmentEvent {
    Remove(usize),
}

impl gpui::EventEmitter<AttachmentEvent> for AttachmentView {}
