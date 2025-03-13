use super::streaming::{DisplayFragment, ProcessorState, StreamProcessor};
use crate::llm::StreamingChunk;
use crate::ui::UserInterface;
use anyhow::Result;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

// A test UI that collects display fragments
pub struct TestUI {
    fragments: Arc<Mutex<VecDeque<DisplayFragment>>>,
}

impl TestUI {
    pub fn new() -> Self {
        Self {
            fragments: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
    
    pub fn get_fragments(&self) -> Vec<DisplayFragment> {
        let guard = self.fragments.lock().unwrap();
        guard.iter().cloned().collect()
    }
}

impl crate::ui::UserInterface for TestUI {
    async fn display(&self, _message: crate::ui::UIMessage) -> Result<(), crate::ui::UIError> {
        Ok(())
    }

    async fn get_input(&self, _prompt: &str) -> Result<String, crate::ui::UIError> {
        Ok(String::new())
    }

    fn display_streaming(&self, _text: &str) -> Result<(), crate::ui::UIError> {
        Ok(())
    }
    
    fn display_fragment(&self, fragment: &DisplayFragment) -> Result<(), crate::ui::UIError> {
        let mut guard = self.fragments.lock().unwrap();
        guard.push_back(fragment.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::StreamingChunk;
    
    #[test]
    fn test_tool_tag_across_chunks() -> Result<()> {
        // Create a test UI
        let test_ui = Arc::new(Box::new(TestUI::new()) as Box<dyn UserInterface>);
        let ui_for_checking = test_ui.downcast_ref::<TestUI>().unwrap();
        
        // Create stream processor
        let mut processor = StreamProcessor::new(test_ui.clone());
        
        // First chunk with partial tool tag
        processor.process(&StreamingChunk::Text(
            "Let me read some files for you using <tool:read".to_string(),
        ))?;
        
        // Second chunk with completion of tool tag and start of param tag
        processor.process(&StreamingChunk::Text(
            "_files><param:paths>[\"src/main.rs\"]</param:paths></tool:read_files>".to_string(),
        ))?;
        
        // Get and verify the fragments
        let fragments = ui_for_checking.get_fragments();
        
        // Find ToolName and ToolParameter fragments
        let mut found_tool_name = false;
        let mut found_tool_param = false;
        let mut tool_id = String::new();
        
        for fragment in &fragments {
            match fragment {
                DisplayFragment::ToolName { name, id } => {
                    found_tool_name = true;
                    assert_eq!(name, "read_files");
                    tool_id = id.clone();
                }
                DisplayFragment::ToolParameter { name, tool_id: param_tool_id, .. } => {
                    found_tool_param = true;
                    assert_eq!(name, "paths");
                    assert_eq!(param_tool_id, &tool_id);
                }
                _ => {}
            }
        }
        
        // Verify we found both tool name and parameter fragments
        assert!(found_tool_name, "Did not find ToolName fragment");
        assert!(found_tool_param, "Did not find ToolParameter fragment");
        
        Ok(())
    }
    
    #[test]
    fn test_complex_tool_call_with_multiple_params() -> Result<()> {
        // Create a test UI
        let test_ui = Arc::new(Box::new(TestUI::new()) as Box<dyn UserInterface>);
        let ui_for_checking = test_ui.downcast_ref::<TestUI>().unwrap();
        
        // Create stream processor with increased buffer size
        let mut processor = StreamProcessor::new(test_ui.clone());
        
        // Simulate chunks with complex tool call
        processor.process(&StreamingChunk::Text(
            "Let me search for specific files <tool:search_fil".to_string(),
        ))?;
        
        processor.process(&StreamingChunk::Text(
            "es><param:query>main function</param:query><param:path>src</".to_string(),
        ))?;
        
        processor.process(&StreamingChunk::Text(
            "param:path><param:case_sensitive>false</param:case_sensitive></tool:search_files>".to_string(),
        ))?;
        
        // Get and verify the fragments
        let fragments = ui_for_checking.get_fragments();
        
        // Find ToolName and ToolParameter fragments
        let mut found_tool_name = false;
        let mut param_count = 0;
        let mut tool_id = String::new();
        
        for fragment in &fragments {
            match fragment {
                DisplayFragment::ToolName { name, id } => {
                    found_tool_name = true;
                    assert_eq!(name, "search_files");
                    tool_id = id.clone();
                }
                DisplayFragment::ToolParameter { name, tool_id: param_tool_id, .. } => {
                    param_count += 1;
                    assert_eq!(param_tool_id, &tool_id);
                }
                _ => {}
            }
        }
        
        // Verify we found tool name and both parameter fragments
        assert!(found_tool_name, "Did not find ToolName fragment");
        assert_eq!(param_count, 3, "Did not find all parameter fragments");
        
        Ok(())
    }
}
