use std::collections::HashSet;

/// Image data returned by tools that produce visual output.
///
/// When a tool returns images via [`Render::render_images`], the agent runner
/// embeds them inside the `ToolResultContent` of the corresponding
/// `ContentBlock::ToolResult`.  For Anthropic, this produces a `content` array
/// containing both text and image blocks per the API spec.
#[derive(Debug, Clone)]
pub struct ImageData {
    /// MIME type, e.g. `"image/png"`, `"image/jpeg"`.
    pub media_type: String,
    /// Base64-encoded image bytes.
    pub base64_data: String,
}

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

    /// Return image data produced by this tool, if any.
    ///
    /// Tools that produce visual output (e.g. `view_images`) override this to
    /// return base64-encoded image data.  The agent runner embeds these inside
    /// the `ToolResultContent` so they are sent as part of the tool result.
    ///
    /// The default implementation returns an empty vec (no images).
    fn render_images(&self) -> Vec<ImageData> {
        Vec::new()
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
