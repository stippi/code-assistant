use std::path::Path;
use std::sync::{Arc, Mutex};

use gpui::{
    div, hsla, prelude::*, px, rgb, App, Context, FocusHandle, Focusable, MouseButton,
    MouseUpEvent, Window,
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

    // Render a single file tree entry item
    fn render_entry_item(
        &self,
        entry: &FileTreeEntry,
        indent_level: usize,
        _cx: &Context<Self>,
    ) -> gpui::Div {
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

        let icon_color = hsla(0., 0., 0.5, 1.0);

        // Create the single item row
        div()
            .py(px(2.))
            .pl(px(indent_level as f32 * 16.0)) // Use 16px indentation per level
            .flex()
            .items_center()
            .gap_2()
            .w_full() // Ensure entry takes full width to prevent wrapping
            .flex_none() // Prevent item from growing or shrinking
            .child(file_icons::render_icon_container(
                &icon,
                16.0,
                icon_color,
                match entry.entry_type {
                    FileSystemEntryType::Directory => "📁",
                    FileSystemEntryType::File => "📄",
                },
            ))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight(400.))
                    .text_color(hsla(0., 0., 0.8, 1.0))
                    .child(entry.name.clone()),
            )
    }

    // Generate a flat list of all file tree entries with proper indentation
    fn generate_file_tree(
        &self,
        entry: &FileTreeEntry,
        indent_level: usize,
        cx: &Context<Self>,
    ) -> Vec<gpui::Div> {
        let mut result = Vec::new();

        // Add current entry
        result.push(self.render_entry_item(entry, indent_level, cx));

        // Add children if expanded
        if entry.is_expanded && !entry.children.is_empty() {
            // Sort children: directories first, then files, both alphabetically
            let mut children: Vec<&FileTreeEntry> = entry.children.values().collect();
            children.sort_by_key(|entry| {
                (
                    // First sort criterion: directories before files
                    matches!(entry.entry_type, FileSystemEntryType::File),
                    // Second sort criterion: alphabetical by name (case insensitive)
                    entry.name.to_lowercase(),
                )
            });

            // Process each child
            for child in children {
                // Recursively add this child and its children
                let child_items = self.generate_file_tree(child, indent_level + 1, cx);
                result.extend(child_items);
            }
        }

        result
    }

    fn generate_resource_section(memory: &WorkingMemory) -> gpui::Stateful<gpui::Div> {
        let resources_header = div()
            .id("resources-header")
            .flex_none()
            .text_sm()
            .w_full()
            .px_2()
            .bg(rgb(0x303030))
            .flex()
            .items_center()
            .justify_between()
            .text_color(hsla(0., 0., 0.9, 1.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(file_icons::render_icon(
                        &file_icons::get().get_library_icon(),
                        16.0,
                        hsla(0., 0., 0.7, 1.0),
                        "📚",
                    ))
                    .child("Loaded Resources"),
            )
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
                    LoadedResource::File(_) => file_icons::get().get_icon(path),
                    LoadedResource::WebSearch { .. } => file_icons::get().get_search_icon(),
                    LoadedResource::WebPage(_) => file_icons::get().get_web_icon(),
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
                    .child(file_icons::render_icon_container(
                        &icon,
                        16.0,
                        hsla(0., 0., 0.7, 1.0),
                        "📄",
                    ))
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

        div()
            .id("resources-section")
            .flex_none()
            .bg(rgb(0x252525))
            .border_b_1()
            .border_color(rgb(0x404040))
            .flex()
            .flex_col()
            .child(resources_header)
            .child(resources_list)
    }

    fn generate_file_tree_section(
        self: &Self,
        memory: &WorkingMemory,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let file_tree_header = div()
            .id("file-tree-header")
            .flex_none()
            .text_sm()
            .w_full()
            .px_2()
            .bg(rgb(0x303030))
            .flex()
            .items_center()
            .justify_between()
            .text_color(hsla(0., 0., 0.9, 1.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(file_icons::render_icon(
                        &file_icons::get().get_file_tree_icon(),
                        16.0,
                        hsla(0., 0., 0.7, 1.0),
                        "🌲",
                    ))
                    .child("File Tree"),
            );

        // File tree content - generate a flat list of items
        let file_tree_content = if let Some(root_entry) = memory.file_tree.as_ref() {
            // Generate flat list of all entries
            let entries = self.generate_file_tree(root_entry, 0, cx);
            div().flex().flex_col().w_full().children(entries)
        } else {
            div()
                .p_2()
                .text_color(hsla(0., 0., 0.5, 1.0))
                .text_center()
                .child("No file tree available")
        };

        let file_tree = div()
            .id("file-tree")
            .overflow_y_scroll()
            .flex_1() // Take remaining space with flex grow
            .min_h(px(100.)) // Minimum height to ensure scrolling works
            .flex()
            .flex_col()
            .p_1()
            .child(file_tree_content);

        div()
            .id("file-tree-section")
            .flex_1() // Take remaining space in parent container
            .min_h(px(100.)) // Minimum height to ensure scrolling works
            .flex()
            .flex_col()
            .child(file_tree_header)
            .child(file_tree)
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
            let resources_section = Self::generate_resource_section(&memory);
            let file_tree_section = self.generate_file_tree_section(&memory, cx);
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
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(if self.is_expanded {
                        file_icons::render_icon(
                            &file_icons::get().get_working_memory_icon(),
                            16.0,
                            hsla(0., 0., 0.7, 1.0),
                            "🧠",
                        )
                    } else {
                        div().into_any_element()
                    })
                    .child(if self.is_expanded {
                        "Working Memory"
                    } else {
                        ""
                    }),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(hsla(0., 0., 0.7, 1.0))
                    .cursor_pointer()
                    .hover(|s| s.text_color(hsla(0., 0., 1.0, 1.0)))
                    .child(file_icons::render_icon(
                        &file_icons::get().get_chevron_icon(self.is_expanded),
                        16.0,
                        hsla(0., 0., 0.7, 1.0),
                        "<",
                    ))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::toggle_sidebar)),
            );

        // Build main container
        let mut container = div()
            .id("memory-sidebar")
            .track_focus(&self.focus_handle(cx))
            .flex_none()
            .w(if self.is_expanded { px(400.) } else { px(40.) })
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
