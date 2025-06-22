use std::path::Path;
use std::sync::{Arc, Mutex};

use gpui::{div, prelude::*, px, App, Axis, Context, Entity, FocusHandle, Focusable, Window};
use gpui_component::{ActiveTheme, StyledExt};

use crate::types::{FileSystemEntryType, FileTreeEntry, LoadedResource, WorkingMemory};
use crate::ui::gpui::file_icons;

// Entity for displaying loaded resources
pub struct LoadedResourcesView {
    memory: Arc<Mutex<Option<WorkingMemory>>>,
    focus_handle: FocusHandle,
}

impl LoadedResourcesView {
    pub fn new(memory: Arc<Mutex<Option<WorkingMemory>>>, cx: &mut Context<Self>) -> Self {
        Self {
            memory,
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Focusable for LoadedResourcesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for LoadedResourcesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let memory_lock = self.memory.lock().unwrap();

        // Resources header (shared between both branches)
        let header = div()
            .id("resources-header")
            .flex_none()
            .text_sm()
            .w_full()
            .px_2()
            .bg(cx.theme().card)
            .flex()
            .items_center()
            .justify_between()
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(file_icons::render_icon(
                        &file_icons::get().get_type_icon(file_icons::LIBRARY),
                        16.0,
                        cx.theme().muted_foreground,
                        "📚",
                    ))
                    .child("Loaded Resources"),
            );

        // Container
        let mut container = div()
            .id("resources-section")
            .flex_none()
            .bg(cx.theme().sidebar)
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
            .flex()
            .flex_col();

        if let Some(memory) = &*memory_lock {
            // Add resources count to header
            let header_with_count = header.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("({})", memory.loaded_resources.len())),
            );

            // Resources content with scrollbar using .scrollable()
            let resources_content = div()
                .id("resources-content")
                .scrollable(Axis::Vertical)
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
                                    cx.theme().muted_foreground,
                                    "📄",
                                ))
                                .child(
                                    div()
                                        .text_color(cx.theme().foreground)
                                        .text_xs()
                                        .truncate()
                                        .flex_grow()
                                        .child(format!("{}/{}", project, path.to_string_lossy())),
                                )
                        }),
                );

            // Update container with the resources content
            container = container.child(header_with_count).child(
                div()
                    .id("resources-list-container")
                    .max_h(px(300.))
                    .flex_grow()
                    .child(resources_content),
            );
        } else {
            // Add empty message with header
            container = container.child(header).child(
                div()
                    .p_2()
                    .text_center()
                    .text_color(cx.theme().muted_foreground)
                    .child("No resources available"),
            );
        }

        container
    }
}

// Entity for displaying file tree
pub struct FileTreeView {
    memory: Arc<Mutex<Option<WorkingMemory>>>,
    focus_handle: FocusHandle,
}

impl FileTreeView {
    pub fn new(memory: Arc<Mutex<Option<WorkingMemory>>>, cx: &mut Context<Self>) -> Self {
        Self {
            memory,
            focus_handle: cx.focus_handle(),
        }
    }

    // Render a single file tree entry item
    fn render_entry_item(
        &self,
        entry: &FileTreeEntry,
        indent_level: usize,
        cx: &Context<Self>,
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

        let icon_color = cx.theme().muted_foreground;

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
                    .text_color(cx.theme().foreground)
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
}

impl Focusable for FileTreeView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FileTreeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let memory_lock = self.memory.lock().unwrap();

        // File tree header (shared between both branches)
        let header = div()
            .id("file-tree-header")
            .flex_none()
            .text_sm()
            .w_full()
            .px_2()
            .bg(cx.theme().card)
            .flex()
            .items_center()
            .justify_between()
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(file_icons::render_icon(
                        &file_icons::get().get_type_icon(file_icons::FILE_TREE),
                        16.0,
                        cx.theme().muted_foreground,
                        "🌲",
                    ))
                    .child("File Tree"),
            );

        // Container for file tree section
        let mut container = div()
            .id("file-tree-section")
            .flex_1() // Take remaining space in parent container
            .min_h(px(100.)) // Minimum height to ensure scrolling works
            .flex()
            .flex_col();

        if let Some(memory) = &*memory_lock {
            // File tree content - generate a flat list of items
            if !memory.file_trees.is_empty() {
                let mut all_entries = Vec::new();
                for (_project_name, root_entry) in &memory.file_trees {
                    // Generate flat list of all entries for this project
                    // The root entry is already the project name
                    let entries = self.generate_file_tree(root_entry, 0, cx);
                    all_entries.extend(entries);
                }

                let file_tree_content = div().flex().flex_col().w_full().children(all_entries);

                // Add the file tree container with scrollable content
                container = container.child(header).child(
                    div()
                        .id("file-tree-container")
                        .flex_1() // Take remaining space in the parent container
                        .min_h(px(100.)) // Minimum height to ensure scrolling works
                        .child(
                            div()
                                .id("file-tree")
                                .size_full()
                                .scrollable(Axis::Vertical)
                                .p_1()
                                .child(file_tree_content),
                        ),
                );
            } else {
                // No file trees
                container = container.child(header).child(
                    div()
                        .p_2()
                        .text_color(cx.theme().muted_foreground)
                        .text_center()
                        .child("No file trees available"),
                );
            }
        } else {
            // No memory data
            container = container.child(header).child(
                div()
                    .p_2()
                    .text_center()
                    .text_color(cx.theme().muted_foreground)
                    .child("No file tree available"),
            );
        }

        container
    }
}

// Memory sidebar component
pub struct MemoryView {
    focus_handle: FocusHandle,
    resources_view: Entity<LoadedResourcesView>,
    file_tree_view: Entity<FileTreeView>,
}

impl MemoryView {
    pub fn new(memory: Arc<Mutex<Option<WorkingMemory>>>, cx: &mut Context<Self>) -> Self {
        // Create sub-entities with the same memory
        let resources_view = cx.new(|cx| LoadedResourcesView::new(memory.clone(), cx));
        let file_tree_view = cx.new(|cx| FileTreeView::new(memory.clone(), cx));

        Self {
            focus_handle: cx.focus_handle(),
            resources_view,
            file_tree_view,
        }
    }
}

impl Focusable for MemoryView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MemoryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title_view = div()
            .id("title-view")
            .flex_none()
            .h(px(36.))
            .w_full()
            .flex()
            .items_center()
            .px_2()
            .justify_between()
            .bg(cx.theme().card)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(file_icons::render_icon(
                        &file_icons::get().get_type_icon(file_icons::WORKING_MEMORY),
                        16.0,
                        cx.theme().muted_foreground,
                        "🧠",
                    ))
                    .child("Working Memory"),
            );

        // Build main container with the title and child entities
        div()
            .id("memory-sidebar")
            .track_focus(&self.focus_handle(cx))
            .flex_none()
            .w(px(260.))
            .h_full()
            .bg(cx.theme().sidebar)
            .overflow_hidden() // Prevent content from overflowing
            .flex()
            .flex_col()
            .child(title_view)
            .child(self.resources_view.clone())
            .child(self.file_tree_view.clone())
    }
}
