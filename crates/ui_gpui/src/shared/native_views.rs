//! Native views laid over a GPUI window, such as the webview of a web
//! editor.
//!
//! A native view is not part of GPUI's element tree: it stays where it was
//! put until it is moved or hidden. Its element moves it each time it is
//! painted ([`Slot::painted`]); this module hides it in a frame that does
//! not paint it (another tab, the settings screen). The root view calls
//! [`begin_frame`] as it renders, which marks the window's views unpainted
//! and checks right after the frame which ones still are.
//!
//! A native view covers everything GPUI draws, popups too. While a select's
//! or combobox's list is open (gpui-component's deferred popovers), native
//! views are hidden, so the list can be seen and picked from.

use gpui_kit::component::GlobalState;
use gpui_kit::{App, Global, Window, WindowId};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// A native view that can be shown and hidden.
pub trait NativeView {
    fn set_shown(&self, shown: bool);
}

/// One registered native view; it is dropped from the registry with the
/// last `Rc`.
pub struct Slot {
    window: WindowId,
    painted: Cell<bool>,
    shown: Cell<bool>,
    view: Box<dyn NativeView>,
}

impl Slot {
    /// The view's element was painted in this frame: show it, unless a
    /// popup list is open.
    pub fn painted(&self, cx: &App) {
        if GlobalState::is_in_deferred_context(cx) {
            return;
        }
        self.painted.set(true);
        if !self.shown.replace(true) {
            self.view.set_shown(true);
        }
    }
}

#[derive(Default)]
struct Registry {
    slots: RefCell<Vec<Weak<Slot>>>,
}

#[derive(Clone, Default)]
struct NativeViews(Rc<Registry>);

impl Global for NativeViews {}

/// Register `view`, shown, in `window`; it is hidden again from the first
/// frame that does not paint it.
pub fn register(view: Box<dyn NativeView>, window: &Window, cx: &mut App) -> Rc<Slot> {
    let slot = Rc::new(Slot {
        window: window.window_handle().window_id(),
        painted: Cell::new(true),
        shown: Cell::new(true),
        view,
    });
    let registry = cx.default_global::<NativeViews>().0.clone();
    let mut slots = registry.slots.borrow_mut();
    slots.retain(|slot| slot.strong_count() > 0);
    slots.push(Rc::downgrade(&slot));
    slot
}

/// A frame of `window` begins: hide its native views that it does not
/// paint. Called by the root view's `render`.
pub fn begin_frame(window: &Window, cx: &App) {
    let Some(registry) = cx.try_global::<NativeViews>().map(|views| views.0.clone()) else {
        return;
    };
    let id = window.window_handle().window_id();
    let slots: Vec<Rc<Slot>> = registry
        .slots
        .borrow()
        .iter()
        .filter_map(Weak::upgrade)
        .filter(|slot| slot.window == id)
        .collect();
    if slots.is_empty() {
        return;
    }
    for slot in &slots {
        slot.painted.set(false);
    }
    window.on_next_frame(move |_, _| {
        for slot in &slots {
            if !slot.painted.get() && slot.shown.replace(false) {
                slot.view.set_shown(false);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{TestAppContext, VisualTestContext, div, prelude::*};

    struct Recorder(Rc<RefCell<Vec<bool>>>);

    impl NativeView for Recorder {
        fn set_shown(&self, shown: bool) {
            self.0.borrow_mut().push(shown);
        }
    }

    struct Empty;

    impl Render for Empty {
        fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui_kit::test]
    fn views_not_painted_in_a_frame_are_hidden(cx: &mut TestAppContext) {
        let window = cx.update(|cx| {
            gpui_kit::component::init(cx);
            cx.open_window(Default::default(), |_, cx| cx.new(|_| Empty))
                .unwrap()
        });
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let slot = cx.update(|window, cx| register(Box::new(Recorder(calls.clone())), window, cx));

        // Painted in its frame: stays shown.
        cx.update(|window, cx| {
            begin_frame(window, cx);
            slot.painted(cx);
            window.simulate_next_frame(cx);
        });
        assert!(calls.borrow().is_empty());

        // A frame without it hides it; the next one painting it shows it.
        cx.update(|window, cx| {
            begin_frame(window, cx);
            window.simulate_next_frame(cx);
            begin_frame(window, cx);
            slot.painted(cx);
            window.simulate_next_frame(cx);
        });
        assert_eq!(*calls.borrow(), [false, true]);

        // Hidden while a popup list is open, shown again once it closes.
        let popover = cx.update(|_, cx| GlobalState::register_deferred_popover(cx));
        cx.update(|window, cx| {
            begin_frame(window, cx);
            slot.painted(cx);
            window.simulate_next_frame(cx);
        });
        drop(popover);
        cx.update(|window, cx| {
            begin_frame(window, cx);
            slot.painted(cx);
            window.simulate_next_frame(cx);
        });
        assert_eq!(*calls.borrow(), [false, true, false, true]);

        // Dropped: no longer tracked.
        drop(slot);
        cx.update(|window, cx| {
            begin_frame(window, cx);
            window.simulate_next_frame(cx);
        });
        assert_eq!(calls.borrow().len(), 4);
    }
}
