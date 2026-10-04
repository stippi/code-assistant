//! The projects view: every session in its project's folder.
//!
//! Folders keep the stable order of [`project_order`]; the user drags a
//! folder onto another to move it. "No project" comes last and stays put.
//! Inside a folder sessions are most recently updated first, settled ones
//! receding. A collapsed folder still marks what inside it wants the user.

use super::project_order;
use super::project_picker::ProjectEntry;
use super::session_item::Attention;
use super::{SessionListItem, SessionSidebar, SessionSidebarEvent, render_hint};
use code_assistant_core::persistence::ChatMetadata;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    AnyElement, AppContext, Context, Entity, Hsla, InteractiveElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px, rems,
};
use std::collections::{HashMap, HashSet};
use tracing::debug;

/// Sessions a folder shows before "Show more".
const FOLDER_PREVIEW: usize = 5;

pub(super) struct ProjectFolder {
    /// `None` holds the sessions started without a project.
    project: Option<String>,
    /// Most recently updated first.
    items: Vec<Entity<SessionListItem>>,
}

impl ProjectFolder {
    /// The key of the folder's collapsed and expanded state.
    fn key(&self) -> &str {
        self.project.as_deref().unwrap_or_default()
    }
}

/// The payload of a folder being dragged.
#[derive(Clone)]
struct DraggedProject {
    name: SharedString,
}

/// What follows the pointer while a folder is dragged.
struct DraggedProjectView {
    name: SharedString,
}

impl Render for DraggedProjectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().drag_border)
            .text_xs()
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .text_color(cx.theme().foreground)
            .child(self.name.clone())
    }
}

impl SessionSidebar {
    /// Rebuild the folders and the picker's projects. Projects seen for the
    /// first time join the stored order at the top.
    pub(super) fn relayout_folders(&mut self, cx: &mut Context<Self>) {
        if !self.sessions_loaded {
            return;
        }
        let known = self.known_projects();
        if project_order::adopt_new_projects(&mut self.project_order, &known) {
            let order = self.project_order.clone();
            crate::update_ui_settings(cx, |settings| settings.sidebar.project_order = order);
        }

        let mut by_project: HashMap<&str, Vec<&ChatMetadata>> = HashMap::new();
        for session in &self.sessions {
            by_project
                .entry(session.initial_project.as_str())
                .or_default()
                .push(session);
        }
        let mut folder_items = |project: &str| -> Vec<Entity<SessionListItem>> {
            let mut sessions = by_project.remove(project).unwrap_or_default();
            sessions.sort_by(|a, b| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then_with(|| b.id.cmp(&a.id))
            });
            sessions
                .into_iter()
                .map(|session| self.items[&session.id].clone())
                .collect()
        };

        let names: HashSet<&str> = known.iter().map(|(name, _)| name.as_str()).collect();
        let mut folders: Vec<ProjectFolder> = project_order::displayed(&self.project_order, &names)
            .into_iter()
            .map(|name| ProjectFolder {
                project: Some(name.to_string()),
                items: folder_items(name),
            })
            .collect();
        let without_project = folder_items("");
        if !without_project.is_empty() {
            folders.push(ProjectFolder {
                project: None,
                items: without_project,
            });
        }

        let entries = folders
            .iter()
            .filter_map(|folder| folder.project.clone())
            .map(|name| ProjectEntry {
                temporary: !self.persisted_projects.contains(&name),
                name,
            })
            .collect();
        self.project_picker
            .update(cx, |picker, cx| picker.set_projects(entries, cx));
        self.folders = folders;
    }

    fn move_project(&mut self, dragged: &str, target: &str, cx: &mut Context<Self>) {
        if project_order::move_project(&mut self.project_order, dragged, target) {
            let order = self.project_order.clone();
            crate::update_ui_settings(cx, |settings| settings.sidebar.project_order = order);
            self.relayout_folders(cx);
            cx.notify();
        }
    }

    fn toggle_folder(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.collapsed_projects.remove(&key) {
            self.collapsed_projects.insert(key);
        }
        let mut collapsed: Vec<String> = self.collapsed_projects.iter().cloned().collect();
        collapsed.sort();
        crate::update_ui_settings(cx, |settings| {
            settings.sidebar.collapsed_projects = collapsed
        });
        cx.notify();
    }

    /// The most urgent mark among the folder's sessions.
    fn folder_attention(&self, folder: &ProjectFolder, cx: &Context<Self>) -> Option<Attention> {
        folder
            .items
            .iter()
            .filter_map(|item| item.read(cx).attention())
            .max()
    }

    pub(super) fn render_folders(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if self.folders.is_empty() {
            return vec![render_hint("No projects yet", cx).into_any_element()];
        }
        let mut children = Vec::new();
        for (index, folder) in self.folders.iter().enumerate() {
            let key = folder.key();
            let collapsed = self.collapsed_projects.contains(key);
            children.push(
                self.render_folder_header(index, folder, collapsed, cx)
                    .into_any_element(),
            );
            if collapsed {
                continue;
            }
            let shown = if self.expanded_lists.contains(key) {
                folder.items.len()
            } else {
                folder.items.len().min(FOLDER_PREVIEW)
            };
            children.extend(
                folder.items[..shown]
                    .iter()
                    .map(|item| item.clone().into_any_element()),
            );
            let hidden = folder.items.len() - shown;
            if hidden > 0 {
                children.push(
                    self.render_show_more(index, key.to_string(), hidden, cx)
                        .into_any_element(),
                );
            }
        }
        children
    }

    fn render_folder_header(
        &self,
        index: usize,
        folder: &ProjectFolder,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let key = folder.key().to_string();
        let title = folder
            .project
            .clone()
            .unwrap_or_else(|| "No project".to_string());
        let is_hovered = self.hovered_folder.as_deref() == Some(key.as_str());
        let is_temporary = folder
            .project
            .as_ref()
            .is_some_and(|name| !self.persisted_projects.contains(name));
        let attention = collapsed
            .then(|| self.folder_attention(folder, cx))
            .flatten()
            .map(|attention| match attention {
                Attention::NeedsApproval => (cx.theme().warning, "Needs approval"),
                Attention::Failed => (cx.theme().danger, "Failed"),
                Attention::Unread => (cx.theme().primary, "Unread"),
            });
        let folder_icon = if collapsed {
            "icons/file_icons/folder.svg"
        } else {
            "icons/file_icons/folder_open.svg"
        };

        div()
            .id(SharedString::from(format!("project-folder-{index}")))
            .w_full()
            .px_2()
            .h(px(28.))
            .mt(if index > 0 { px(6.) } else { px(0.) })
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .rounded_sm()
            .on_hover(cx.listener({
                let key = key.clone();
                move |this, hovered: &bool, _, cx| {
                    // Leaving a folder must not clear a neighbour that was
                    // entered first.
                    let hovered_here = this.hovered_folder.as_deref() == Some(key.as_str());
                    if *hovered && !hovered_here {
                        this.hovered_folder = Some(key.clone());
                        cx.notify();
                    } else if !*hovered && hovered_here {
                        this.hovered_folder = None;
                        cx.notify();
                    }
                }
            }))
            .on_click(cx.listener({
                let key = key.clone();
                move |this, _, _, cx| this.toggle_folder(key.clone(), cx)
            }))
            // Named folders move by drag and drop; "No project" stays last.
            .when_some(folder.project.clone(), |el, name| {
                let name = SharedString::from(name);
                el.on_drag(
                    DraggedProject { name: name.clone() },
                    |dragged, _, _, cx| {
                        cx.new(|_| DraggedProjectView {
                            name: dragged.name.clone(),
                        })
                    },
                )
                .drag_over::<DraggedProject>(|style, _, _, cx| style.bg(cx.theme().drop_target))
                .on_drop(cx.listener(
                    move |this, dragged: &DraggedProject, _, cx| {
                        this.move_project(&dragged.name, &name, cx)
                    },
                ))
            })
            .child(
                gpui_kit::svg()
                    .flex_none()
                    .size(rems(0.875))
                    .path(folder_icon)
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(cx.theme().foreground)
                    .child(SharedString::from(title.clone())),
            )
            .children(attention.map(|(color, label)| attention_dot(index, color, label)))
            .when(is_temporary, |el| {
                let project = folder.project.clone().unwrap_or_default();
                el.child(
                    self.folder_button(
                        format!("pin-project-{index}"),
                        "icons/pin.svg",
                        "Temporary project — save to make it a first-class project \
                         that can be referenced by tool calls in other sessions"
                            .to_string(),
                        is_hovered.then(|| cx.theme().muted_foreground),
                        cx,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(SessionSidebarEvent::PersistProjectRequested {
                            project_name: project.clone(),
                        });
                    })),
                )
            })
            .child({
                let project = folder.project.clone();
                self.folder_button(
                    format!("new-session-in-{index}"),
                    "icons/plus.svg",
                    format!("New session in {title}"),
                    is_hovered.then(|| cx.theme().primary),
                    cx,
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    debug!("New session in {:?}", project);
                    cx.emit(SessionSidebarEvent::NewSessionRequested {
                        name: None,
                        initial_project: project.clone(),
                    });
                }))
            })
    }

    /// An icon button on a folder header; it keeps its place while hidden
    /// (`color` is `None`) so hovering does not shift the title.
    fn folder_button(
        &self,
        id: String,
        icon: &'static str,
        tooltip: String,
        color: Option<Hsla>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let visible = color.is_some();
        div()
            .id(SharedString::from(id))
            .flex_none()
            .size(rems(1.25))
            .rounded_sm()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(visible, |el| {
                el.hover(|s| s.bg(cx.theme().muted))
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            })
            .child(
                gpui_kit::svg()
                    .size(rems(0.75))
                    .path(icon)
                    .text_color(color.unwrap_or(cx.theme().transparent)),
            )
    }

    fn render_show_more(
        &self,
        index: usize,
        key: String,
        hidden: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("show-more-{index}")))
            .w_full()
            .pl(px(26.))
            .pr_2()
            .py(px(4.))
            .cursor_pointer()
            .rounded_sm()
            .hover(|s| s.bg(cx.theme().muted.opacity(0.3)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.expanded_lists.insert(key.clone());
                cx.notify();
            }))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground.opacity(0.8))
                    .child(SharedString::from(format!("Show {hidden} more"))),
            )
    }
}

/// The mark on a collapsed folder whose sessions want the user.
fn attention_dot(index: usize, color: Hsla, label: &'static str) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("folder-attention-{index}")))
        .flex_none()
        .size(rems(1.25))
        .flex()
        .items_center()
        .justify_center()
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
        .child(div().size(px(6.)).rounded_full().bg(color))
}
