//! System message generation functionality

use crate::agent::ToolSyntax;
use crate::tools::core::ToolScope;
use crate::tools::ParserRegistry;

const SYSTEM_MESSAGE: &str = include_str!("../../resources/system_message.md");
const SYSTEM_MESSAGE_TOOLS: &str = include_str!("../../resources/system_message_tools.md");

/// Generate a system message for the given tool syntax and scope
pub fn generate_system_message(tool_syntax: ToolSyntax, scope: ToolScope) -> String {
    match tool_syntax {
        ToolSyntax::Native => SYSTEM_MESSAGE.to_string(),
        _ => {
            // For XML and Caret modes, get the base template and replace placeholders
            let mut base = SYSTEM_MESSAGE_TOOLS.to_string();

            // Get parser and generate syntax-specific documentation
            let parser = ParserRegistry::get(tool_syntax);

            // Replace syntax documentation
            if let Some(syntax_doc) = parser.generate_syntax_documentation() {
                base = base.replace("{{syntax}}", &syntax_doc);
            } else {
                base = base.replace("{{syntax}}", "");
            }

            // Replace tools documentation
            if let Some(tools_doc) = parser.generate_tool_documentation(scope) {
                base = base.replace("{{tools}}", &tools_doc);
            } else {
                base = base.replace("{{tools}}", "");
            }

            base
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_message_generation() {
        // Test Native mode
        let native_msg = generate_system_message(ToolSyntax::Native, ToolScope::Agent);
        assert!(!native_msg.contains("{{tools}}"));
        assert!(!native_msg.contains("{{syntax}}"));
        assert!(!native_msg.contains("<tool:"));
        assert!(!native_msg.contains("^^^"));
        println!("✓ Native system message OK ({} chars)", native_msg.len());

        // Test XML mode
        let xml_msg = generate_system_message(ToolSyntax::Xml, ToolScope::Agent);
        assert!(
            !xml_msg.contains("{{tools}}"),
            "XML message contains unreplaced {{tools}} placeholder"
        );
        assert!(
            !xml_msg.contains("{{syntax}}"),
            "XML message contains unreplaced {{syntax}} placeholder"
        );
        assert!(
            xml_msg.contains("<tool:"),
            "XML message should contain XML syntax examples"
        );
        assert!(
            !xml_msg.contains("triple-caret"),
            "XML message should not contain caret syntax docs"
        );
        println!("✓ XML system message OK ({} chars)", xml_msg.len());

        // Test Caret mode
        let caret_msg = generate_system_message(ToolSyntax::Caret, ToolScope::Agent);
        assert!(
            !caret_msg.contains("{{tools}}"),
            "Caret message contains unreplaced {{tools}} placeholder"
        );
        assert!(
            !caret_msg.contains("{{syntax}}"),
            "Caret message contains unreplaced {{syntax}} placeholder"
        );
        assert!(
            caret_msg.contains("^^^"),
            "Caret message should contain caret syntax examples"
        );
        assert!(
            !caret_msg.contains("<tool:"),
            "Caret message should not contain XML syntax docs"
        );
        println!("✓ Caret system message OK ({} chars)", caret_msg.len());

        // Verify syntax-specific content
        assert!(
            xml_msg.contains("XML-style tags"),
            "XML message should explain XML syntax"
        );
        assert!(
            caret_msg.contains("triple-caret fenced blocks"),
            "Caret message should explain caret syntax"
        );
    }
}
