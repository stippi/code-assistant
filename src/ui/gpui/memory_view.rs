use std::sync::{Arc, Mutex};

use gpui::{
    div, hsla, prelude::*, px, rgb, MouseButton, MouseUpEvent, Context, Window, 
    FocusHandle, Focusable, App
};

use crate::types::{FileSystemEntryType, FileTreeEntry, LoadedResource, WorkingMemory};

// Memory sidebar component
pub struct MemoryView {
    is_expanded: bool,
    memory: Arc<Mutex<Option<WorkingMemory>>>,
    focus_handle: FocusHandle,
}

impl MemoryView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            is_expanded: true,
            memory: Arc::new(Mutex::new(None)),
            focus_handle: cx.focus_handle(),
        }
    }

    // Toggle the expanded state of the sidebar
    fn toggle_sidebar(&mut self, _: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.is_expanded = !self.is_expanded;
        cx.notify();
    }

    // Update the memory model
    pub fn update_memory(&mut self, memory: WorkingMemory, cx: &mut Context<Self>) {
        *self.memory.lock().unwrap() = Some(memory);
        cx.notify();
    }

    // Render a file tree entry using a simpler approach
    fn render_file_tree_entry(&self, entry: &FileTreeEntry, indent_level: usize) -> gpui::Div {
        let indent = indent_level * 16;
        
        // Create base div
        let mut base = div()
            .py_1()
            .pl(px(indent as f32))
            .flex()
            .items_center()
            .gap_1()
            .child(
                // Icon based on type
                div()
                    .text_color(match entry.entry_type {
                        FileSystemEntryType::Directory => hsla(210., 0.7, 0.7, 1.0), // Blue
                        FileSystemEntryType::File => hsla(0., 0., 0.7, 1.0),        // Light gray
                    })
                    .child(match entry.entry_type {
                        FileSystemEntryType::Directory => if entry.is_expanded { "📂" } else { "📁" },
                        FileSystemEntryType::File => "📄",
                    })
            )
            .child(div().text_color(hsla(0., 0., 0.9, 1.0)).child(entry.name.clone()));
        
        // Add children if expanded
        if entry.is_expanded && !entry.children.is_empty() {
            // Create child elements
            let children: Vec<gpui::Div> = entry.children
                .values()
                .map(|child| self.render_file_tree_entry(child, indent_level + 1))
                .collect();
                
            // Add a container with all children
            base = base.child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .children(children)
            );
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
                .child(div().text_xs().text_color(hsla(0., 0., 0.6, 1.0))
                    .child(format!("({})", memory.loaded_resources.len())));
                
            let resources_list = div()
                .id("resources-list")
                .max_h(px(300.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .p_1()
                .gap_1()
                .children(
                    memory.loaded_resources.iter().map(|(path, resource)| {
                        div()
                            .rounded_sm()
                            .p_2()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .bg(rgb(0x303030))
                            .border_l_4()
                            .border_color(match resource {
                                LoadedResource::File(_) => rgb(0x4CAF50), // Green
                                LoadedResource::WebSearch { .. } => rgb(0x2196F3), // Blue
                                LoadedResource::WebPage(_) => rgb(0x9C27B0), // Purple
                            })
                            .child(
                                div()
                                    .text_color(hsla(0., 0., 0.9, 1.0))
                                    .text_sm()
                                    .truncate()
                                    .child(path.to_string_lossy().to_string())
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(hsla(0., 0., 0.6, 1.0))
                                    .child(match resource {
                                        LoadedResource::File(_) => "File",
                                        LoadedResource::WebSearch { .. } => "Web Search",
                                        LoadedResource::WebPage(_) => "Web Page",
                                    })
                            )
                    })
                );
                
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
                self.render_file_tree_entry(memory.file_tree.as_ref().unwrap(), 0)
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
        
        // Toggle button
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
            .child(if self.is_expanded { "Memory Explorer" } else { "" })
            .child(
                div()
                    .text_lg()
                    .px_2()
                    .py_1()
                    .text_color(hsla(0., 0., 0.7, 1.0))
                    .cursor_pointer()
                    .hover(|s| s.text_color(hsla(0., 0., 1.0, 1.0)))
                    .child(if self.is_expanded { "◀" } else { "▶" })
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::toggle_sidebar))
            );
        
        // Build main container
        let mut container = div()
            .id("memory-sidebar")
            .track_focus(&self.focus_handle(cx))
            .flex_none()
            .w_full() // Take full width of the parent container
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
