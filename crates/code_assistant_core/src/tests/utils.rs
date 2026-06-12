use crate::tools::ToolRequest;
use crate::types::ToolSyntax;
use anyhow::Result;

/// Parse tool requests from LLM response and return both requests and truncated response after first tool
/// This is a wrapper that defaults to XML parsing for backward compatibility
pub fn parse_and_truncate_llm_response(
    response: &llm::LLMResponse,
    request_id: u64,
) -> Result<(Vec<ToolRequest>, llm::LLMResponse)> {
    // Default to XML parser for backward compatibility with existing tests
    let parser = crate::tool_dialects::dialect_for(ToolSyntax::Xml);
    parser.extract_requests(response, request_id, 0, &crate::tools::test_registry())
}
