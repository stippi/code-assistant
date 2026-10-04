//! The popover behind the sidebar's "+": where does the new session start?
//!
//! A header with a search field and the "+ Project" button, then the
//! projects most recently active first and "No project" last. Typing
//! filters, Up/Down move the highlight, Enter picks it, Escape closes.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size, ThemeStyled};
use gpui_kit::{
    Animation, AnimationExt, AnyElement, App, BoxShadow, Context, Entity, EventEmitter,
    FocusHandle, Focusable, Hsla, InteractiveElement, KeyDownEvent, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, hsla, prelude::*, px, rems,
};
use std::time::Duration;

/// How long the surface takes to settle, shadcn/ui's `animate-in` figure.
const ENTER_DURATION: Duration = Duration::from_millis(150);
/// Where the surface starts, above where it comes to rest.
const ENTER_OFFSET: f32 = -8.;

/// The picker on a popup surface with gpui-component's dropdown motion: over
/// 150ms the surface fades in while sliding the last 8px down out of the
/// trigger. GPUI has no group compositing, so the ring and shadow rise with
/// the cube of the fade; otherwise they would show through the translucent
/// panel as a dark slab. gpui-component keeps this motion crate-private
/// (`popover::dropdown_popup`) for Select, Combobox and DatePicker; its
/// plain `Popover` has none, so it is mirrored here.
pub fn animated_surface(picker: Entity<ProjectPicker>, cx: &App) -> AnyElement {
    // Read out here: the animation runs long after `cx` is gone.
    let ring = cx.theme().foreground.alpha(0.1);
    div()
        .occlude()
        .popover_style(cx)
        .child(picker)
        .with_animation(
            "project-picker-enter",
            Animation::new(ENTER_DURATION).with_easing(ease_out_cubic),
            move |surface, delta| {
                surface
                    .top(px(ENTER_OFFSET * (1. - delta)))
                    .opacity(delta)
                    .shadow(surface_shadow(ring, delta * delta * delta))
            },
        )
        .into_any_element()
}

fn ease_out_cubic(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

/// shadcn/ui's popup shadow — a hairline ring plus `shadow-md` — at `strength`
/// of its full ink, as gpui-component draws it for its own dropdowns.
fn surface_shadow(ring: Hsla, strength: f32) -> Vec<BoxShadow> {
    let strength = strength.clamp(0., 1.);
    let ink = hsla(0., 0., 0., 0.1 * strength);
    vec![
        BoxShadow::new(px(0.), px(0.), ring.alpha(ring.a * strength))
            .blur_radius(px(0.))
            .spread_radius(px(1.)),
        BoxShadow::new(px(0.), px(4.), ink)
            .blur_radius(px(3.))
            .spread_radius(px(-1.)),
        BoxShadow::new(px(0.), px(2.), ink)
            .blur_radius(px(2.))
            .spread_radius(px(-2.)),
    ]
}

const ROW_HEIGHT: f32 = 28.;
const LIST_PADDING: f32 = 4.;

#[derive(Clone, Debug)]
pub enum ProjectPickerEvent {
    /// Start a session in the project, or without one.
    Picked {
        project: Option<String>,
    },
    AddProjectRequested,
    /// Escape: close without picking.
    Dismissed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectEntry {
    pub name: String,
    /// Known only from sessions, not saved in projects.json.
    pub temporary: bool,
}

/// One row of the filtered list.
#[derive(Clone)]
enum Row {
    Project(ProjectEntry),
    NoProject,
}

pub struct ProjectPicker {
    input: Entity<InputState>,
    projects: Vec<ProjectEntry>,
    highlighted: usize,
    focus_handle: FocusHandle,
    _input_subscription: Subscription,
}

impl ProjectPicker {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search projects"));
        let subscription = cx.subscribe_in(&input, window, Self::on_input_event);
        Self {
            input,
            projects: Vec::new(),
            highlighted: 0,
            focus_handle: cx.focus_handle(),
            _input_subscription: subscription,
        }
    }

    /// The search field's focus handle; the popover stays open while it
    /// has focus.
    pub fn input_focus_handle(&self, cx: &gpui_kit::App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }

    pub fn set_projects(&mut self, projects: Vec<ProjectEntry>, cx: &mut Context<Self>) {
        if self.projects != projects {
            self.projects = projects;
            self.highlighted = 0;
            cx.notify();
        }
    }

    /// Prepare for opening: empty search, first row highlighted, focus in
    /// the search field.
    pub fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.highlighted = 0;
        cx.notify();
    }

    fn rows(&self, cx: &gpui_kit::App) -> Vec<Row> {
        let query = self.input.read(cx).value().trim().to_lowercase();
        let mut rows: Vec<Row> = self
            .projects
            .iter()
            .filter(|p| query.is_empty() || p.name.to_lowercase().contains(&query))
            .cloned()
            .map(Row::Project)
            .collect();
        if query.is_empty() || "no project".contains(&query) {
            rows.push(Row::NoProject);
        }
        rows
    }

    fn pick(&mut self, row: &Row, cx: &mut Context<Self>) {
        let project = match row {
            Row::Project(entry) => Some(entry.name.clone()),
            Row::NoProject => None,
        };
        cx.emit(ProjectPickerEvent::Picked { project });
    }

    fn pick_highlighted(&mut self, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        if let Some(row) = rows.get(self.highlighted.min(rows.len().saturating_sub(1))) {
            let row = row.clone();
            self.pick(&row, cx);
        }
    }

    fn on_input_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.highlighted = 0;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.pick_highlighted(cx),
            InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.rows(cx).len();
        match event.keystroke.key.as_str() {
            "down" if count > 0 => {
                self.highlighted = (self.highlighted + 1) % count;
            }
            "up" if count > 0 => {
                self.highlighted = (self.highlighted + count - 1) % count;
            }
            "escape" => cx.emit(ProjectPickerEvent::Dismissed),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn render_row(&self, index: usize, row: &Row, cx: &mut Context<Self>) -> impl IntoElement {
        let highlighted = index == self.highlighted;
        let (icon, title, temporary) = match row {
            Row::Project(entry) => (
                "icons/file_icons/folder.svg",
                entry.name.clone(),
                entry.temporary,
            ),
            Row::NoProject => ("icons/file_generic.svg", "No project".to_string(), false),
        };
        let row_for_click = row.clone();
        div()
            .id(SharedString::from(format!("project-pick-{index}")))
            .w_full()
            .px_2()
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap_2()
            .rounded_sm()
            .cursor_pointer()
            .when(highlighted, |el| el.bg(cx.theme().muted.opacity(0.5)))
            .when(!highlighted, |el| {
                el.hover(|s| s.bg(cx.theme().muted.opacity(0.3)))
            })
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered && this.highlighted != index {
                    this.highlighted = index;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.pick(&row_for_click, cx)))
            .child(
                gpui_kit::svg()
                    .flex_none()
                    .size(rems(0.75))
                    .path(icon)
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(SharedString::from(title)),
            )
            .when(temporary, |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground.opacity(0.7))
                        .child("temporary"),
                )
            })
    }
}

impl EventEmitter<ProjectPickerEvent> for ProjectPicker {}

impl Focusable for ProjectPicker {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProjectPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows(cx);
        if self.highlighted >= rows.len() {
            self.highlighted = rows.len().saturating_sub(1);
        }
        // The scroll area needs a definite height to scroll at all: the rows
        // are 28px each, the list caps at half the window.
        let content_height = rows.len().max(1) as f32 * ROW_HEIGHT + 2. * LIST_PADDING;
        let list_height = px(content_height.min(f32::from(window.viewport_size().height) * 0.5));
        let row_elements: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| self.render_row(index, row, cx).into_any_element())
            .collect();

        div()
            .id("project-picker")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .w(px(260.))
            .flex()
            .flex_col()
            // Header: search and the add-project button
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .py(px(6.))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.input)
                                .with_size(Size::Small)
                                .prefix(
                                    Icon::default()
                                        .path(SharedString::from("icons/magnifying_glass.svg"))
                                        .with_size(Size::XSmall)
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .appearance(false)
                                .p_0(),
                        ),
                    )
                    .child(
                        div()
                            .id("project-picker-add")
                            .flex_none()
                            .h(px(24.))
                            .px_2()
                            .rounded_sm()
                            .border_1()
                            .border_color(cx.theme().border)
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor_pointer()
                            .hover(|s| s.bg(cx.theme().muted))
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(ProjectPickerEvent::AddProjectRequested)
                            }))
                            .child(
                                gpui_kit::svg()
                                    .size(rems(0.7))
                                    .path("icons/plus.svg")
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().foreground)
                                    .child("Project"),
                            ),
                    ),
            )
            // Filtered projects
            .child(
                div()
                    .id("project-picker-list")
                    .w_full()
                    .h(list_height)
                    .p(px(LIST_PADDING))
                    .overflow_y_scrollbar()
                    .flex()
                    .flex_col()
                    .children(row_elements)
                    .when(rows.is_empty(), |el| {
                        el.child(
                            div()
                                .px_2()
                                .py_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("No matching project"),
                        )
                    }),
            )
    }
}
