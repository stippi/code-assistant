use std::collections::HashSet;

/// Responsible for formatting tool outputs for display
pub trait Render: Send + Sync + 'static {
    /// Generate a short status message for display in action history
    fn status(&self) -> String;

    /// Format the detailed output for LLM context (tool result in conversation)
    /// The resources_tracker helps detect and handle redundant output
    fn render(&self, resources_tracker: &mut ResourcesTracker) -> String;

    /// Format the output for UI display in tool blocks.
    /// By default, returns the same as render().
    /// Override this for tools that need different UI representation (e.g., spawn_agent
    /// returns JSON for custom rendering while render() returns plain text for LLM).
    fn render_for_ui(&self, resources_tracker: &mut ResourcesTracker) -> String {
        self.render(resources_tracker)
    }
}

/// Tracks resources that have been included in tool outputs to prevent redundant display
pub struct ResourcesTracker {
    /// Set of already rendered resource identifiers
    rendered_resources: HashSet<String>,
}

impl ResourcesTracker {
    /// Create a new empty resources tracker
    pub fn new() -> Self {
        Self {
            rendered_resources: HashSet::new(),
        }
    }

    /// Check if a resource has already been rendered
    pub fn is_rendered(&self, resource_id: &str) -> bool {
        self.rendered_resources.contains(resource_id)
    }

    /// Mark a resource as rendered to prevent duplicate display
    pub fn mark_rendered(&mut self, resource_id: String) {
        self.rendered_resources.insert(resource_id);
    }
}
