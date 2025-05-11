use gpui::{div, prelude::*, AnyView, App, Context, Entity, IntoElement, Render, Window};
use gpui_component::Root;

// A component that renders the Root and all its layers (drawer, modal, notifications)
pub struct RootRenderer {
    root: Entity<Root>,
}

impl RootRenderer {
    pub fn new(root: Entity<Root>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { root }
    }
}

impl Render for RootRenderer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Get the drawer, modal, and notification layers from the Root
        let drawer_layer = Root::render_drawer_layer(window, cx);
        let modal_layer = Root::render_modal_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);

        // The Root view itself
        let root_view = self.root.clone();

        // Combine all elements
        div()
            .size_full()
            .child(root_view)
            .children(drawer_layer)
            .children(modal_layer)
            .children(notification_layer)
    }
}
