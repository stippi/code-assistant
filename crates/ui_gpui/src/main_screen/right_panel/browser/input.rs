//! The user's mouse and keyboard, forwarded to the page while they have
//! control: mouse and wheel on the frame, keys on the focused panel, text an
//! input method composes through [`EntityInputHandler`], and copy, cut and
//! paste through the system clipboard.

use super::keys::{self, KeyAction};
use super::{BrowserPanel, geometry};
use gpui_kit::{
    Bounds, ClipboardItem, Context, Div, EntityInputHandler, InteractiveElement, KeyDownEvent,
    KeyUpEvent, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    ScrollWheelEvent, Stateful, Styled, UTF16Selection, Window, px,
};
use std::ops::Range;
use web::{Button, Point, UserInput};

impl BrowserPanel {
    /// The page point under a window position, if the frame shows one there.
    fn page_point(&self, position: gpui_kit::Point<Pixels>) -> Option<Point> {
        let meta = self.frame.as_ref()?.meta;
        let bounds = self.frame_bounds.get()?;
        let local = position - bounds.origin;
        let view = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let (x, y) = geometry::to_page(&meta, view, (f32::from(local.x), f32::from(local.y)))?;
        Some(Point { x, y })
    }

    pub(super) fn with_mouse_input(
        &self,
        surface: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        if !self.user_in_control() {
            return surface;
        }
        let surface = [MouseButton::Left, MouseButton::Right, MouseButton::Middle]
            .into_iter()
            .fold(surface, |surface, button| {
                surface
                    .on_mouse_down(
                        button,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            window.focus(&this.focus_handle, cx);
                            this.mouse_down(event);
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_up(
                        button,
                        cx.listener(|this, event: &MouseUpEvent, _, _| this.mouse_up(event)),
                    )
                    .on_mouse_up_out(
                        button,
                        cx.listener(|this, event: &MouseUpEvent, _, _| this.mouse_up(event)),
                    )
            });
        surface
            .cursor_default()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, _| {
                if let Some(at) = this.page_point(event.position) {
                    this.send(UserInput::MouseMove {
                        at,
                        buttons: this.held_buttons,
                        modifiers: keys::modifier_mask(&event.modifiers),
                    });
                }
            }))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                let Some(at) = this.page_point(event.position) else {
                    return;
                };
                // GPUI's delta moves the content; the page's moves the view.
                let delta = event.delta.pixel_delta(window.line_height());
                this.send(UserInput::Wheel {
                    at,
                    dx: -f64::from(f32::from(delta.x)),
                    dy: -f64::from(f32::from(delta.y)),
                    modifiers: keys::modifier_mask(&event.modifiers),
                });
                cx.stop_propagation();
            }))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent) {
        let (Some(at), Some((button, mask))) =
            (self.page_point(event.position), page_button(event.button))
        else {
            return;
        };
        self.held_buttons |= mask;
        self.send(UserInput::MouseDown {
            at,
            button,
            click_count: event.click_count as u32,
            buttons: self.held_buttons,
            modifiers: keys::modifier_mask(&event.modifiers),
        });
    }

    fn mouse_up(&mut self, event: &MouseUpEvent) {
        let Some((button, mask)) = page_button(event.button) else {
            return;
        };
        if self.held_buttons & mask == 0 {
            return;
        }
        self.held_buttons &= !mask;
        // A release outside the frame lands on its nearest edge.
        let at = self.page_point(event.position).unwrap_or_else(|| {
            self.clamped_page_point(event.position)
                .unwrap_or(Point { x: 0.0, y: 0.0 })
        });
        self.send(UserInput::MouseUp {
            at,
            button,
            click_count: event.click_count as u32,
            buttons: self.held_buttons,
            modifiers: keys::modifier_mask(&event.modifiers),
        });
    }

    fn clamped_page_point(&self, position: gpui_kit::Point<Pixels>) -> Option<Point> {
        let bounds = self.frame_bounds.get()?;
        let inner = |v: Pixels, lo: Pixels, len: Pixels| v.clamp(lo, lo + len - px(1.));
        self.page_point(gpui_kit::point(
            inner(position.x, bounds.origin.x, bounds.size.width),
            inner(position.y, bounds.origin.y, bounds.size.height),
        ))
    }

    pub(super) fn with_key_input(&self, panel: Div, cx: &mut Context<Self>) -> Div {
        if !self.user_in_control() {
            return panel;
        }
        panel
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                // The address field types for itself.
                if this.address.is_some() || this.marked_text.is_some() {
                    return;
                }
                if this.key_down(event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, _| {
                let key = &event.keystroke.key;
                if this.keys_down.remove(key) {
                    this.send(UserInput::KeyUp {
                        key: key.clone(),
                        modifiers: keys::modifier_mask(&event.keystroke.modifiers),
                    });
                }
            }))
    }

    /// Handle a key press; `false` leaves it to the app.
    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) -> bool {
        let keystroke = &event.keystroke;
        match keys::key_down(keystroke) {
            KeyAction::Pass => false,
            KeyAction::Send {
                key,
                text,
                commands,
            } => {
                self.keys_down.insert(key.clone());
                self.send(UserInput::KeyDown {
                    key,
                    text,
                    modifiers: keys::modifier_mask(&keystroke.modifiers),
                    commands,
                });
                true
            }
            KeyAction::Paste => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.send(UserInput::InsertText(text));
                }
                true
            }
            action @ (KeyAction::Copy | KeyAction::Cut) => {
                let cut = action == KeyAction::Cut;
                self.copy_selection(cut, keystroke.modifiers, cx);
                true
            }
        }
    }

    /// Put the page's selection on the clipboard; `cut` removes it after.
    fn copy_selection(&self, cut: bool, modifiers: Modifiers, cx: &mut Context<Self>) {
        let Some(input) = self.input.clone() else {
            return;
        };
        cx.spawn(async move |_, cx| {
            let Ok(text) = input.selected_text().await else {
                return;
            };
            if text.is_empty() {
                return;
            }
            cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(text)));
            if cut {
                input.send(UserInput::KeyDown {
                    key: "Delete".into(),
                    text: None,
                    modifiers: keys::modifier_mask(&modifiers) & !4,
                    commands: vec![],
                });
                input.send(UserInput::KeyUp {
                    key: "Delete".into(),
                    modifiers: 0,
                });
            }
        })
        .detach();
    }
}

/// The page's button and its mask bit for a GPUI button.
fn page_button(button: MouseButton) -> Option<(Button, i64)> {
    match button {
        MouseButton::Left => Some((Button::Left, 1)),
        MouseButton::Right => Some((Button::Right, 2)),
        MouseButton::Middle => Some((Button::Middle, 4)),
        _ => None,
    }
}

/// Text from an input method: composed text is held until committed, then
/// inserted into the page. The page's own text is not mirrored, so ranges
/// are empty.
impl EntityInputHandler for BrowserPanel {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self
            .marked_text
            .as_ref()
            .map_or(0, |text| text.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_text
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        if !text.is_empty() && self.address.is_none() {
            self.send(UserInput::InsertText(text.to_string()));
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!new_text.is_empty()).then(|| new_text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui_kit::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
