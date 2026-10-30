//! A comment's body as GitHub renders it ([`markdown::prepare`]). Its
//! Markdown is parsed once, when it is first shown, and kept with the card
//! that owns it; a long body is clipped at 240 px under a fade, with "Show
//! full comment" to unfold it.

use super::super::markdown;
use github::RepoRef;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::{
    AnyElement, Context, Entity, Hsla, Pixels, SharedString, div, linear_color_stop,
    linear_gradient, prelude::*, px,
};

/// Taller bodies are clipped.
pub const CLIP_HEIGHT: Pixels = px(240.);
const FADE_HEIGHT: Pixels = px(40.);

pub struct CommentBody {
    id: SharedString,
    /// The body prepared for the Markdown view.
    text: SharedString,
    /// Parsed on first render.
    state: Option<Entity<TextViewState>>,
    /// Long bodies are clipped (not a description).
    clip: bool,
    expanded: bool,
    /// The last layout was taller than [`CLIP_HEIGHT`].
    overflowing: bool,
}

impl CommentBody {
    /// `id` keys the body's elements; `text` is GitHub Markdown.
    pub fn new(id: impl Into<SharedString>, text: &str, repo: &RepoRef) -> Self {
        Self {
            id: id.into(),
            text: markdown::prepare(text, repo).into(),
            state: None,
            clip: true,
            expanded: false,
            overflowing: false,
        }
    }

    /// Shown whole, however long.
    pub fn unclipped(mut self) -> Self {
        self.clip = false;
        self
    }

    /// The body changed (an edit, a refresh); a shown body is parsed anew.
    pub fn set_text<V>(&mut self, text: &str, repo: &RepoRef, cx: &mut Context<V>) {
        let prepared = markdown::prepare(text, repo);
        if self.text.as_ref() == prepared {
            return;
        }
        if let Some(state) = &self.state {
            state.update(cx, |state, cx| state.set_text(&prepared, cx));
        }
        self.text = prepared.into();
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    #[cfg(test)]
    pub fn is_mounted(&self) -> bool {
        self.state.is_some()
    }

    /// The body inside a card painted in `background` (the fade's colour).
    /// `body` finds this body in its owner `V`.
    pub fn render<V: 'static>(
        &mut self,
        background: Hsla,
        body: fn(&mut V) -> &mut CommentBody,
        cx: &mut Context<V>,
    ) -> AnyElement {
        let text = self.text.clone();
        let state = self
            .state
            .get_or_insert_with(|| cx.new(|cx| TextViewState::markdown(&text, cx).selectable(true)))
            .clone();
        let clipped = self.clip && !self.expanded;
        let fade = clipped && self.overflowing;
        let owner = cx.entity().downgrade();
        let id = self.id.clone();
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .min_w_0()
                    .when(clipped, |el| el.max_h(CLIP_HEIGHT).overflow_hidden())
                    // The content keeps its height inside the clip; tell the
                    // owner whether it is taller.
                    .on_children_prepainted(move |bounds, _, cx| {
                        let Some(content) = bounds.first() else {
                            return;
                        };
                        let overflowing = content.size.height > CLIP_HEIGHT;
                        let owner = owner.clone();
                        cx.defer(move |cx| {
                            owner
                                .update(cx, |owner, cx| {
                                    let shown = body(owner);
                                    if shown.overflowing != overflowing {
                                        shown.overflowing = overflowing;
                                        cx.notify();
                                    }
                                })
                                .ok();
                        });
                    })
                    .child(div().min_w_0().child(TextView::new(&state)))
                    .when(fade, |el| {
                        el.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(FADE_HEIGHT)
                                .bg(linear_gradient(
                                    180.,
                                    linear_color_stop(background.opacity(0.), 0.),
                                    linear_color_stop(background, 1.),
                                )),
                        )
                    }),
            )
            .when(self.clip && self.overflowing, |el| {
                el.child(
                    div().pt_2().child(
                        Button::new(SharedString::from(format!("{id}-full")))
                            .ghost()
                            .xsmall()
                            .label(if self.expanded {
                                "Show less"
                            } else {
                                "Show full comment"
                            })
                            .text_color(cx.theme().muted_foreground)
                            .on_click(cx.listener(move |owner, _, _, cx| {
                                let shown = body(owner);
                                shown.expanded = !shown.expanded;
                                cx.notify();
                            })),
                    ),
                )
            })
            .into_any_element()
    }
}
