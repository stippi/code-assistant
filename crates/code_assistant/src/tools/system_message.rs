//! System message generation functionality

use crate::agent::ToolSyntax;
use crate::tools::core::ToolScope;
use crate::tools::ParserRegistry;

const SYSTEM_MESSAGE: &str = include_str!("../../resources/system_message.md");
const TOOLS_INTRODUCTION: &str = include_str!("../../resources/tool_use_intro.md");

/// Generate a system message for the given tool syntax and scope
pub fn generate_system_message(tool_syntax: ToolSyntax, scope: ToolScope) -> String {
    let mut base = SYSTEM_MESSAGE.to_string();

    match tool_syntax {
        ToolSyntax::Native => {
            // For native mode, replace the entire placeholder section with an empty line
            base = base.replace("{{syntax}}\n\n{{tools}}\n\n", "\n");
            base
        }
        _ => {
            // For XML and Caret modes, get parser and generate documentation
            let parser = ParserRegistry::get(tool_syntax);

            let tool_use_header = "TOOL USE\n\n";

            // Replace syntax documentation with tools introduction + syntax doc
            if let Some(syntax_doc) = parser.generate_syntax_documentation() {
                let syntax_content = format!("{TOOLS_INTRODUCTION}{syntax_doc}");
                base = base.replace("{{syntax}}", &syntax_content);
            } else {
                base = base.replace("{{syntax}}", TOOLS_INTRODUCTION);
            }

            // Replace tools documentation with header + tools doc
            if let Some(tools_doc) = parser.generate_tool_documentation(scope) {
                let tools_content = format!("{tool_use_header}{tools_doc}");
                base = base.replace("{{tools}}", &tools_content);
            } else {
                base = base.replace("{{tools}}", tool_use_header);
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
        assert!(!native_msg.contains("TOOL USE"));
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
        assert!(
            xml_msg.contains("TOOL USE"),
            "XML message should contain tool use sections"
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
        assert!(
            caret_msg.contains("TOOL USE"),
            "Caret message should contain tool use sections"
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
