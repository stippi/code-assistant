
use std::sync::Arc;

// Mock implementations for testing
struct MockProjectManager;
struct MockCommandExecutor;
struct MockUI;
struct MockStatePersistence;

impl code_assistant::config::ProjectManager for MockProjectManager {
    fn get_projects(&self) -> anyhow::Result<std::collections::HashMap<String, std::path::PathBuf>> {
        Ok(std::collections::HashMap::new())
    }
    
    fn add_temporary_project(&mut self, _path: std::path::PathBuf) -> anyhow::Result<String> {
        Ok("test-project".to_string())
    }
    
    fn get_explorer_for_project(&self, _name: &str) -> anyhow::Result<Box<dyn code_assistant::types::CodeExplorer>> {
        unimplemented!()
    }
}

impl code_assistant::utils::CommandExecutor for MockCommandExecutor {}

impl code_assistant::ui::UserInterface for MockUI {
    async fn get_input(&self) -> Result<String, code_assistant::ui::UIError> {
        Ok("test".to_string())
    }
    
    fn display_fragment(&self, _fragment: &code_assistant::ui::streaming::DisplayFragment) -> Result<(), code_assistant::ui::UIError> {
        Ok(())
    }
    
    async fn send_event(&self, _event: code_assistant::ui::UiEvent) -> Result<(), code_assistant::ui::UIError> {
        Ok(())
    }
    
    fn should_streaming_continue(&self) -> bool {
        true
    }
}

impl code_assistant::agent::persistence::AgentStatePersistence for MockStatePersistence {
    fn save_agent_state(
        &mut self,
        _messages: Vec<llm::Message>,
        _tool_executions: Vec<code_assistant::agent::ToolExecution>,
        _working_memory: code_assistant::types::WorkingMemory,
        _init_path: Option<std::path::PathBuf>,
        _initial_project: Option<String>,
        _next_request_id: u64,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    
    fn load_agent_state(&self) -> anyhow::Result<Option<(Vec<llm::Message>, Vec<code_assistant::agent::ToolExecution>, code_assistant::types::WorkingMemory, Option<std::path::PathBuf>, Option<String>, Option<u64>)>> {
        Ok(None)
    }
}

fn main() {
    use code_assistant::agent::{Agent, ToolSyntax};
    use code_assistant::tools::ParserRegistry;
    use code_assistant::tools::core::ToolScope;
    
    // Test XML documentation
    let xml_parser = ParserRegistry::get(ToolSyntax::Xml);
    if let Some(xml_docs) = xml_parser.generate_tool_documentation(ToolScope::Agent) {
        println!("=== XML Tool Documentation ===");
        println!("{}", &xml_docs[..500.min(xml_docs.len())]);
        println!("...\n");
    }
    
    // Test Caret documentation
    let caret_parser = ParserRegistry::get(ToolSyntax::Caret);
    if let Some(caret_docs) = caret_parser.generate_tool_documentation(ToolScope::Agent) {
        println!("=== Caret Tool Documentation ===");
        println!("{}", &caret_docs[..500.min(caret_docs.len())]);
        println!("...\n");
    }
    
    // Test Native documentation (should be None)
    let native_parser = ParserRegistry::get(ToolSyntax::Native);
    if let Some(_) = native_parser.generate_tool_documentation(ToolScope::Agent) {
        println!("Native parser unexpectedly returned documentation");
    } else {
        println!("=== Native Tool Documentation ===");
        println!("None (as expected - uses API tool definitions)\n");
    }
}
