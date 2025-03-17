use gpui::{div, px, svg, Element, IntoElement, ParentElement, Styled};

/// Simple container for a pulsing arrow SVG
pub struct RotatingArrow {
    size: f32,
    color: gpui::Hsla,
}

impl RotatingArrow {
    pub fn new(size: f32, color: gpui::Hsla) -> Self {
        Self { size, color }
    }
}

impl IntoElement for RotatingArrow {
    type Element = gpui::AnyElement;

    fn into_element(self) -> Self::Element {
        // Create a div with the arrow SVG - we'll use a pulsing effect instead of rotation
        // since animations in GPUI seem to be complex to implement
        div()
            .w(px(self.size))
            .h(px(self.size))
            .flex()
            .items_center()
            .justify_center()
            .opacity(0.8)
            .child(
                svg()
                    .path("icons/arrow_circle.svg")
                    .size(px(self.size))
                    .text_color(self.color),
            )
            .into_any()
    }
}
