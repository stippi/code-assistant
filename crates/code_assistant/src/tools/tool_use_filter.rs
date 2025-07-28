//! Tool use filtering system to control which tool blocks are allowed and when to truncate responses

/// Trait for filtering tool use blocks during parsing
/// This allows controlling which tools can be used and when to stop parsing
pub trait ToolUseFilter: Send + Sync {
    /// Called when a tool block has been completed successfully
    /// Returns whether additional content (including more tool blocks) should be allowed after this tool
    /// If false, the response will be truncated immediately after this tool block
    fn allow_content_after_tool(&self, tool_name: &str, tool_count: usize) -> bool;

    /// Called when a new tool block is encountered and the tool name has been parsed
    /// Returns whether this tool block should be allowed at this position
    /// If false, the response will be truncated before this tool block
    fn allow_tool_at_position(&self, tool_name: &str, tool_count: usize) -> bool;
}

/// Default filter that allows only one tool per message
/// This prevents the LLM from chaining multiple tools before seeing the results
pub struct SingleToolFilter;

impl ToolUseFilter for SingleToolFilter {
    fn allow_content_after_tool(&self, _tool_name: &str, _tool_count: usize) -> bool {
        // After any tool completes, no additional content is allowed
        false
    }

    fn allow_tool_at_position(&self, _tool_name: &str, tool_count: usize) -> bool {
        // Only allow the first tool (tool_count starts at 1)
        tool_count == 1
    }
}

/// Filter that allows unlimited tools (for backwards compatibility or special cases)
pub struct UnlimitedToolFilter;

impl ToolUseFilter for UnlimitedToolFilter {
    fn allow_content_after_tool(&self, _tool_name: &str, _tool_count: usize) -> bool {
        true
    }

    fn allow_tool_at_position(&self, _tool_name: &str, _tool_count: usize) -> bool {
        true
    }
}

/// Smart filter that prevents certain tool combinations that don't make logical sense
/// For example, prevents file editing tools before file reading tools have had their results processed
pub struct SmartToolFilter {}

impl SmartToolFilter {
    pub fn new() -> Self {
        Self {}
    }

    /// Check if a tool is a "read" operation (doesn't modify state)
    fn is_read_tool(&self, tool_name: &str) -> bool {
        matches!(
            tool_name,
            "read_files"
                | "name_session"
                | "list_files"
                | "list_projects"
                | "search_files"
                | "web_fetch"
                | "web_search"
        )
    }
}

impl ToolUseFilter for SmartToolFilter {
    fn allow_content_after_tool(&self, tool_name: &str, _tool_count: usize) -> bool {
        // Allow content after read tools, but not after write tools
        // This allows the LLM to potentially chain multiple read tools
        self.is_read_tool(tool_name)
    }

    fn allow_tool_at_position(&self, tool_name: &str, tool_count: usize) -> bool {
        // First tool is always allowed
        if tool_count == 1 {
            return true;
        }

        // Allow read tools after other read tools (e.g., read file then list directory)
        // But don't allow write tools after any tool (they need to see results first)
        self.is_read_tool(tool_name)
    }
}

impl Default for SmartToolFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_tool_filter() {
        let filter = SingleToolFilter;

        // First tool should be allowed
        assert!(filter.allow_tool_at_position("read_files", 1));

        // Second tool should not be allowed
        assert!(!filter.allow_tool_at_position("list_files", 2));

        // No content after any tool
        assert!(!filter.allow_content_after_tool("read_files", 1));
    }

    #[test]
    fn test_unlimited_tool_filter() {
        let filter = UnlimitedToolFilter;

        // All tools should be allowed
        assert!(filter.allow_tool_at_position("read_files", 1));
        assert!(filter.allow_tool_at_position("list_files", 2));
        assert!(filter.allow_tool_at_position("write_file", 3));

        // Content after all tools should be allowed
        assert!(filter.allow_content_after_tool("read_files", 1));
        assert!(filter.allow_content_after_tool("write_file", 2));
    }

    #[test]
    fn test_smart_tool_filter() {
        let filter = SmartToolFilter::new();

        // First tool always allowed
        assert!(filter.allow_tool_at_position("read_files", 1));
        assert!(filter.allow_tool_at_position("write_file", 1));

        // Read tools can follow other tools
        assert!(filter.allow_tool_at_position("list_files", 2));
        assert!(filter.allow_tool_at_position("search_files", 2));
        assert!(filter.allow_tool_at_position("read_files", 2));
        assert!(filter.allow_tool_at_position("web_fetch", 2));

        // Write tools should not be allowed as second tool
        assert!(!filter.allow_tool_at_position("write_file", 2));
        assert!(!filter.allow_tool_at_position("replace_in_file", 2));
        assert!(!filter.allow_tool_at_position("delete_files", 2));
        assert!(!filter.allow_tool_at_position("execute_command", 2));

        // Content allowed after read tools but not write tools
        assert!(filter.allow_content_after_tool("read_files", 1));
        assert!(filter.allow_content_after_tool("list_files", 1));
        assert!(!filter.allow_content_after_tool("write_file", 1));
        assert!(!filter.allow_content_after_tool("replace_in_file", 1));
    }

    #[test]
    fn test_tool_classification() {
        let filter = SmartToolFilter::new();

        // Test read tool classification
        assert!(filter.is_read_tool("read_files"));
        assert!(filter.is_read_tool("list_files"));
        assert!(filter.is_read_tool("search_files"));
        assert!(filter.is_read_tool("web_fetch"));
        assert!(filter.is_read_tool("web_search"));
        assert!(!filter.is_read_tool("write_file"));
        assert!(!filter.is_read_tool("replace_in_file"));
        assert!(!filter.is_read_tool("delete_files"));
        assert!(!filter.is_read_tool("execute_command"));
    }
}
