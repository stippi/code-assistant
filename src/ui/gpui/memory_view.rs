use std::path::Path;
use std::sync::{Arc, Mutex};

use gpui::{
    div, hsla, prelude::*, px, rgb, App, Context, FocusHandle, Focusable, MouseButton,
    MouseUpEvent, SharedString, Window,
};

use crate::types::{FileSystemEntryType, FileTreeEntry, LoadedResource, WorkingMemory};
use crate::ui::gpui::file_icons;

// Memory sidebar component
pub struct MemoryView {
    is_expanded: bool,
    memory: Arc<Mutex<Option<WorkingMemory>>>,
    focus_handle: FocusHandle,
}

impl MemoryView {
    pub fn new(memory: Arc<Mutex<Option<WorkingMemory>>>, cx: &mut Context<Self>) -> Self {
        Self {
            is_expanded: true,
            memory,
            focus_handle: cx.focus_handle(),
        }
    }

    // Toggle the expanded state of the sidebar
    fn toggle_sidebar(&mut self, _: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.is_expanded = !self.is_expanded;
        cx.notify();
    }

    // Render a file tree entry using a simpler approach
    fn render_file_tree_entry(&self, entry: &FileTreeEntry, indent_level: usize, cx: &Context<Self>) -> gpui::Div {
        let indent = indent_level;
        
        // Get appropriate icon based on type and name
        let icon = match entry.entry_type {
            FileSystemEntryType::Directory => {
                // Get folder icon based on expanded state
                file_icons::get().get_folder_icon(entry.is_expanded)
            }
            FileSystemEntryType::File => {
                // Get file icon based on file extension
                let path = Path::new(&entry.name);
                file_icons::get().get_icon(path)
            }
        };

        // Create base div
        let mut base = div()
            .py_1()
            .pl(px(indent as f32 * 16.0)) // Use 16px indentation per level
            .flex()
            .items_center()
            .gap_2()
            .child(
                // Icon container
                div()
                    .w(px(16.0))
                    .h(px(16.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(match entry.entry_type {
                        FileSystemEntryType::Directory => hsla(210., 0.7, 0.7, 1.0), // Blue
                        FileSystemEntryType::File => hsla(0., 0., 0.7, 1.0),         // Light gray
                    })
                    .child(icon),
            )
            .child(
                div()
                    .text_color(hsla(0., 0., 0.9, 1.0))
                    .child(entry.name.clone()),
            );

        // Add children if expanded
        if entry.is_expanded && !entry.children.is_empty() {
            // Sort children: directories first, then files, all alphabetically
            let mut children: Vec<&FileTreeEntry> = entry.children.values().collect();
            children.sort_by(|a, b| {
                match (&a.entry_type, &b.entry_type) {
                    (FileSystemEntryType::Directory, FileSystemEntryType::File) => std::cmp::Ordering::Less,
                    (FileSystemEntryType::File, FileSystemEntryType::Directory) => std::cmp::Ordering::Greater,
                    _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                }
            });
            
            // Create child elements
            let child_elements: Vec<gpui::Div> = children
                .iter()
                .map(|child| self.render_file_tree_entry(child, indent_level + 1, cx))
                .collect();

            // Add a container with all children
            base = base.child(div().flex().flex_col().w_full().children(child_elements));
        }

        base
    }
}

impl Focusable for MemoryView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MemoryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_memory = self.memory.lock().unwrap().is_some();
        // Use expanded state to control content visibility, not width

        // Create components that will be used in when blocks
        let memory_content = if self.is_expanded && has_memory {
            let memory = self.memory.lock().unwrap().clone().unwrap();

            // Resources section
            let resources_header = div()
                .id("resources-header")
                .flex_none()
                .h(px(28.))
                .w_full()
                .px_2()
                .bg(rgb(0x303030))
                .flex()
                .items_center()
                .justify_between()
                .text_color(hsla(0., 0., 0.9, 1.0))
                .child("Loaded Resources")
                .child(
                    div()
                        .text_xs()
                        .text_color(hsla(0., 0., 0.6, 1.0))
                        .child(format!("({})", memory.loaded_resources.len())),
                );

            let resources_list = div()
                .id("resources-list")
                .max_h(px(300.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .p_1()
                .gap_1()
                .children(memory.loaded_resources.iter().map(|(path, resource)| {
                    // Get appropriate icon for resource type
                    let icon = match resource {
                        LoadedResource::File(_) => {
                            file_icons::get().get_icon(path)
                        },
                        LoadedResource::WebSearch { .. } => {
                            SharedString::from("🔍")
                        },
                        LoadedResource::WebPage(_) => {
                            SharedString::from("🌐")
                        }
                    };
                    
                    div()
                        .rounded_sm()
                        .py_1()
                        .px_2()
                        .w_full()
                        .flex()
                        .bg(rgb(0x303030))
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .w_full()
                        .child(
                            div()
                                .w(px(16.0))
                                .h(px(16.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon)
                        )
                        .child(
                            div()
                                .text_color(hsla(0., 0., 0.8, 1.0))
                                .text_sm()
                                .truncate()
                                .flex_grow()
                                .child(path.to_string_lossy().to_string()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(hsla(0., 0., 0.5, 1.0))
                                .flex_none()
                                .child(match resource {
                                    LoadedResource::File(_) => "File",
                                    LoadedResource::WebSearch { .. } => "Web Search",
                                    LoadedResource::WebPage(_) => "Web Page",
                                }),
                        )
                }));

            let resources_section = div()
                .id("resources-section")
                .flex_none()
                .bg(rgb(0x252525))
                .border_b_1()
                .border_color(rgb(0x404040))
                .flex()
                .flex_col()
                .child(resources_header)
                .child(resources_list);

            // File tree header
            let file_tree_header = div()
                .id("file-tree-header")
                .flex_none()
                .h(px(28.))
                .w_full()
                .px_2()
                .bg(rgb(0x303030))
                .flex()
                .items_center()
                .justify_between()
                .text_color(hsla(0., 0., 0.9, 1.0))
                .child("File Tree");

            // File tree content
            let file_tree_content = if memory.file_tree.is_some() {
                self.render_file_tree_entry(memory.file_tree.as_ref().unwrap(), 0, cx)
            } else {
                div()
                    .p_2()
                    .text_color(hsla(0., 0., 0.5, 1.0))
                    .text_center()
                    .child("No file tree available")
            };

            let file_tree = div()
                .id("file-tree")
                .flex_1()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .p_1()
                .child(file_tree_content);

            let file_tree_section = div()
                .id("file-tree-section")
                .flex_1()
                .flex()
                .flex_col()
                .child(file_tree_header)
                .child(file_tree);

            Some((resources_section, file_tree_section))
        } else {
            None
        };

        // Toggle button with SVG icon for expansion indicator
        let toggle_button = div()
            .id("sidebar-toggle")
            .flex_none()
            .h(px(36.))
            .w_full()
            .flex()
            .items_center()
            .px_2()
            .justify_between()
            .bg(rgb(0x303030))
            .text_color(hsla(0., 0., 0.8, 1.0))
            .child(if self.is_expanded {
                "Working Memory"
            } else {
                ""
            })
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(hsla(0., 0., 0.7, 1.0))
                    .cursor_pointer()
                    .hover(|s| s.text_color(hsla(0., 0., 1.0, 1.0)))
                    .child(file_icons::get().get_arrow_icon(self.is_expanded))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::toggle_sidebar)),
            );

        // Build main container
        let mut container = div()
            .id("memory-sidebar")
            .track_focus(&self.focus_handle(cx))
            .flex_none()
            .w(px(280.)) // Fixed width matching parent container's allocation
            .h_full()
            .bg(rgb(0x252525))
            .border_l_1()
            .border_color(rgb(0x404040))
            .overflow_hidden() // Prevent content from overflowing
            .flex()
            .flex_col()
            .child(toggle_button);

        // Add memory content if available
        if let Some((resources_section, file_tree_section)) = memory_content {
            container = container.child(resources_section).child(file_tree_section);
        }

        container
    }
}
