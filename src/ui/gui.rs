use crate::ui::{UIError, UIMessage, UserInterface};
use async_trait::async_trait;
use std::ops::Range;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::{self, Receiver, Sender};

use gpui::{
    actions, black, div, fill, hsla, opaque_grey, point, prelude::*, px, relative, rgb, rgba, size,
    white, yellow, App, Application, Bounds, ClipboardItem, Context, CursorStyle, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable, GlobalElementId,
    KeyBinding, Keystroke, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    P