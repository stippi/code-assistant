use std::path::Path;
use std::sync::{Arc, Mutex};

use gpui::{
    div, hsla, prelude::*, px, rgb, App, Context, FocusHandle, Focusable, ScrollHandle, Window,
};

use super::scrollbar::{Scrollbar, ScrollbarState};
use crate::types::{FileSystemEntryType, FileTreeEntry, LoadedResource, WorkingMemory};
use crate::ui::gpui::file_icons;

// Memory sidebar component
pub struct MemoryView {
    memory: Arc<Mutex<Option<WorkingMemory>>>,
    focus_handle: FocusHandle,
    resources_scroll_handle: ScrollHandle,
    file_tree_scroll_handle: ScrollHandle,
}

impl MemoryView {
    pub fn new(memory: Arc<Mutex<Option<WorkingMemory>>>, cx: &mut Context<Self>) -> Self {
        Self {
            memory,
            focus_handle: cx.focus_handle(),
            // Initialize scroll handles
            resources_scroll_handle: ScrollHandle::new(),
            file_tree_scroll_handle: ScrollHandle::new(),
        }
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
                let icon_type = if entry.is_expanded {
                    file_icons::DIRECTORY_EXPANDED
                } else {
                    file_icons::DIRECTORY_COLLAPSED
                };
                file_icons::get().get_type_icon(icon_type)
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

    fn generate_resource_section(
        &self,
        memory: &WorkingMemory,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
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
                        &file_icons::get().get_type_icon(file_icons::LIBRARY),
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

        // Create scrollbar state for resources
        let resources_scrollbar_state =
            ScrollbarState::new(self.resources_scroll_handle.clone()).parent_entity(&cx.entity());

        // Resources list with content (resources)
        let resources_content = div()
            .id("resources-content")
            .flex()
            .flex_col()
            .p_1()
            .gap_1()
            .children(
                memory
                    .loaded_resources
                    .iter()
                    .map(|((project, path), resource)| {
                        // Get appropriate icon for resource type
                        let icon = match resource {
                            LoadedResource::File(_) => file_icons::get().get_icon(path),
                            LoadedResource::WebSearch { .. } => {
                                file_icons::get().get_type_icon(file_icons::MAGNIFYING_GLASS)
                            }
                            LoadedResource::WebPage(_) => {
                                file_icons::get().get_type_icon(file_icons::HTML)
                            }
                        };

                        div()
                            .px_2()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(file_icons::render_icon_container(
                                &icon,
                                14.0,
                                hsla(0., 0., 0.7, 1.0),
                                "📄",
                            ))
                            .child(
                                div()
                                    .text_color(hsla(0., 0., 0.8, 1.0))
                                    .text_xs()
                                    .truncate()
                                    .flex_grow()
                                    .child(format!("{}/{}", project, path.to_string_lossy())),
                            )
                    }),
            );

        // Container with scrollable area and scrollbar
        let resources_container = div()
            .id("resources-list-container")
            .relative() // For absolute positioning of scrollbar
            .max_h(px(300.))
            .flex_grow()
            .child(
                div()
                    .id("resources-list")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.resources_scroll_handle)
                    .child(resources_content),
            )
            .child(
                // Add scrollbar
                match Scrollbar::vertical(resources_scrollbar_state) {
                    Some(scrollbar) => div()
                        .absolute()
                        .right(px(0.))
                        .top(px(0.))
                        .h_full()
                        .w(px(12.))
                        .child(scrollbar)
                        .into_any_element(),
                    None => div().w(px(0.)).h(px(0.)).into_any_element(),
                },
            );

        div()
            .id("resources-section")
            .flex_none()
            .bg(rgb(0x252525))
            .border_b_1()
            .border_color(rgb(0x404040))
            .flex()
            .flex_col()
            .child(resources_header)
            .child(resources_container)
    }

    fn generate_file_tree_section(
        &self,
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
                        &file_icons::get().get_type_icon(file_icons::FILE_TREE),
                        16.0,
                        hsla(0., 0., 0.7, 1.0),
                        "🌲",
                    ))
                    .child("File Tree"),
            );

        // File tree content - generate a flat list of items
        let file_tree_content = if !memory.file_trees.is_empty() {
            let mut all_entries = Vec::new();
            for (_project_name, root_entry) in &memory.file_trees {
                // Generate flat list of all entries for this project
                // The root entry is already the project name
                let entries = self.generate_file_tree(root_entry, 0, cx);
                all_entries.extend(entries);
            }
            div().flex().flex_col().w_full().children(all_entries)
        } else {
            div()
                .p_2()
                .text_color(hsla(0., 0., 0.5, 1.0))
                .text_center()
                .child("No file trees available")
        };

        // Create scrollbar state for file tree
        let file_tree_scrollbar_state =
            ScrollbarState::new(self.file_tree_scroll_handle.clone()).parent_entity(&cx.entity());

        // Container with scrollable area and scrollbar
        let file_tree_container = div()
            .id("file-tree-container")
            .relative() // For absolute positioning of scrollbar
            .flex_1() // Take remaining space in the parent container
            .min_h(px(100.)) // Minimum height to ensure scrolling works
            .child(
                div()
                    .id("file-tree")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.file_tree_scroll_handle)
                    .p_1()
                    .child(file_tree_content),
            )
            .child(
                // Add scrollbar
                match Scrollbar::vertical(file_tree_scrollbar_state) {
                    Some(scrollbar) => div()
                        .absolute()
                        .right(px(0.))
                        .top(px(0.))
                        .h_full()
                        .w(px(12.))
                        .child(scrollbar)
                        .into_any_element(),
                    None => div().w(px(0.)).h(px(0.)).into_any_element(),
                },
            );

        div()
            .id("file-tree-section")
            .flex_1() // Take remaining space in parent container
            .min_h(px(100.)) // Minimum height to ensure scrolling works
            .flex()
            .flex_col()
            .child(file_tree_header)
            .child(file_tree_container)
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
        let memory_content = if has_memory {
            let memory = self.memory.lock().unwrap().clone().unwrap();
            let resources_section = self.generate_resource_section(&memory, cx);
            let file_tree_section = self.generate_file_tree_section(&memory, cx);
            Some((resources_section, file_tree_section))
        } else {
            None
        };

        let title_view = div()
            .id("title-view")
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
                    .child(file_icons::render_icon(
                        &file_icons::get().get_type_icon(file_icons::WORKING_MEMORY),
                        16.0,
                        hsla(0., 0., 0.7, 1.0),
                        "🧠",
                    ))
                    .child("Working Memory"),
            );

        // Build main container
        let mut container = div()
            .id("memory-sidebar")
            .track_focus(&self.focus_handle(cx))
            .flex_none()
            .w(px(260.))
            .h_full()
            .bg(rgb(0x252525))
            .border_l_1()
            .border_color(rgb(0x404040))
            .overflow_hidden() // Prevent content from overflowing
            .flex()
            .flex_col()
            .child(title_view);

        // Add memory content if available
        if let Some((resources_section, file_tree_section)) = memory_content {
            container = container.child(resources_section).child(file_tree_section);
        }

        container
    }
}
