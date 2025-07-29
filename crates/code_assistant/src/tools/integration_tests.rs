#[cfg(test)]
mod tests {
    use crate::tools::core::ToolRegistry;
    use std::env;

    #[test]
    fn test_default_tool_registration() {
        // Clear environment variable to ensure default behavior
        env::remove_var("CODE_ASSISTANT_USE_DIFF_FORMAT");
        
        // Create a new registry to test default registration
        let mut registry = ToolRegistry::new();
        registry.register_default_tools(false); // Default behavior
        
        // Edit tool should be registered
        assert!(registry.get("edit").is_some(), "Edit tool should be registered by default");
        
        // Replace in file tool should NOT be registered
        assert!(registry.get("replace_in_file").is_none(), "ReplaceInFile tool should not be registered by default");
        
        // Other core tools should still be registered
        assert!(registry.get("read_files").is_some(), "ReadFiles tool should be registered");
        assert!(registry.get("write_file").is_some(), "WriteFile tool should be registered");
        assert!(registry.get("list_files").is_some(), "ListFiles tool should be registered");
    }

    #[test]
    fn test_diff_format_tool_registration() {
        // Create a new registry to test diff format registration
        let mut registry = ToolRegistry::new();
        registry.register_default_tools(true); // Use diff format
        
        // Replace in file tool should be registered
        assert!(registry.get("replace_in_file").is_some(), "ReplaceInFile tool should be registered with diff format");
        
        // Edit tool should NOT be registered
        assert!(registry.get("edit").is_none(), "Edit tool should not be registered with diff format");
        
        // Other core tools should still be registered
        assert!(registry.get("read_files").is_some(), "ReadFiles tool should be registered");
        assert!(registry.get("write_file").is_some(), "WriteFile tool should be registered");
        assert!(registry.get("list_files").is_some(), "ListFiles tool should be registered");
    }

    #[test]
    fn test_tool_specs() {
        // Test edit tool spec
        let mut registry = ToolRegistry::new();
        registry.register_default_tools(false);
        
        if let Some(edit_tool) = registry.get("edit") {
            let spec = edit_tool.spec();
            assert_eq!(spec.name, "edit");
            assert!(spec.description.contains("Edit a file by replacing specific text content"));
            
            // Check required parameters
            let params = &spec.parameters_schema;
            let properties = params.get("properties").unwrap().as_object().unwrap();
            assert!(properties.contains_key("project"));
            assert!(properties.contains_key("path"));
            assert!(properties.contains_key("old_text"));
            assert!(properties.contains_key("new_text"));
            
            // Check that replace_all is optional
            let required = params.get("required").unwrap().as_array().unwrap();
            assert!(required.contains(&serde_json::Value::String("project".to_string())));
            assert!(required.contains(&serde_json::Value::String("path".to_string())));
            assert!(required.contains(&serde_json::Value::String("old_text".to_string())));
            assert!(required.contains(&serde_json::Value::String("new_text".to_string())));
            assert!(!required.contains(&serde_json::Value::String("replace_all".to_string())));
        } else {
            panic!("Edit tool should be registered");
        }
    }

    #[test]
    fn test_replace_in_file_tool_specs() {
        // Test replace_in_file tool spec
        let mut registry = ToolRegistry::new();
        registry.register_default_tools(true);
        
        if let Some(replace_tool) = registry.get("replace_in_file") {
            let spec = replace_tool.spec();
            assert_eq!(spec.name, "replace_in_file");
            assert!(spec.description.contains("Replace sections in a file"));
            
            // Check required parameters
            let params = &spec.parameters_schema;
            let properties = params.get("properties").unwrap().as_object().unwrap();
            assert!(properties.contains_key("project"));
            assert!(properties.contains_key("path"));
            assert!(properties.contains_key("diff"));
            
            let required = params.get("required").unwrap().as_array().unwrap();
            assert!(required.contains(&serde_json::Value::String("project".to_string())));
            assert!(required.contains(&serde_json::Value::String("path".to_string())));
            assert!(required.contains(&serde_json::Value::String("diff".to_string())));
        } else {
            panic!("ReplaceInFile tool should be registered with diff format");
        }
    }
}
