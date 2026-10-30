//! Round GitHub avatars. The image is asked for at twice the shown size
//! (sharp on Retina, small to load); the login's initial stands in while it
//! loads, when it fails and when there is none.

use github::model::Actor;
use gpui_kit::component::ActiveTheme;
use gpui_kit::{AnyElement, App, Pixels, SharedString, div, img, prelude::*};

/// `url` asking GitHub for an image `size` pixels wide.
fn sized_url(url: &str, size: u32) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}s={size}")
}

pub fn avatar(actor: Option<&Actor>, size: Pixels, cx: &App) -> AnyElement {
    let login = actor.map_or("ghost", |a| a.login.as_str());
    let initial: SharedString = login
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default()
        .into();
    let (bg, fg) = (cx.theme().muted, cx.theme().muted_foreground);
    let placeholder = move || {
        div()
            .flex_none()
            .size(size)
            .rounded_full()
            .bg(bg)
            .flex()
            .items_center()
            .justify_center()
            .text_size(size * 0.6)
            .text_color(fg)
            .child(initial.clone())
            .into_any_element()
    };
    match actor.and_then(|a| a.avatar_url.as_deref()) {
        Some(url) => {
            let pixels = (f32::from(size) * 2.).ceil() as u32;
            img(SharedString::from(sized_url(url, pixels)))
                .flex_none()
                .size(size)
                .rounded_full()
                .with_loading(placeholder.clone())
                .with_fallback(placeholder)
                .into_any_element()
        }
        None => placeholder(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_for_a_small_image() {
        assert_eq!(
            sized_url("https://avatars.githubusercontent.com/u/1?v=4", 40),
            "https://avatars.githubusercontent.com/u/1?v=4&s=40"
        );
        assert_eq!(
            sized_url("https://avatars.githubusercontent.com/in/15368", 28),
            "https://avatars.githubusercontent.com/in/15368?s=28"
        );
    }
}
