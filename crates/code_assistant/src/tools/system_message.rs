//! System message generation functionality

use crate::agent::ToolSyntax;
use crate::tools::core::ToolScope;
use crate::tools::ParserRegistry;
use rust_embed::RustEmbed;
use serde::Deserialize;
use std::sync::OnceLock;
use tracing::warn;

const TOOLS_INTRODUCTION: &str = include_str!("../../resources/tool_use_intro.md");
const DEFAULT_PROMPT_FALLBACK: &str = include_str!("../../resources/system_prompts/default.md");

#[derive(RustEmbed)]
#[folder = "resources/system_prompts"]
struct EmbeddedSystemPrompts;

#[derive(Debug)]
struct PromptMapping {
    default_prompt: String,
    prompts: Vec<ModelPrompt>,
}

#[derive(Debug)]
struct ModelPrompt {
    file: String,
    model_substrings: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawPromptMapping {
    #[serde(default = "default_prompt_name")]
    default_prompt: String,
    #[serde(default)]
    prompts: Vec<RawModelPrompt>,
}

#[derive(Debug, Deserialize)]
struct RawModelPrompt {
    file: String,
    #[serde(default)]
    model_substrings: Vec<String>,
}

fn default_prompt_name() -> String {
    "default.md".to_string()
}

static PROMPT_MAPPING: OnceLock<PromptMapping> = OnceLock::new();

/// Generate a system message for the given tool syntax, scope, and optional model hint
pub fn generate_system_message(
    tool_syntax: ToolSyntax,
    scope: ToolScope,
    model_hint: Option<&str>,
) -> String {
    let mut base = load_base_prompt(model_hint);

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

fn load_base_prompt(model_hint: Option<&str>) -> String {
    let mapping = PROMPT_MAPPING.get_or_init(load_prompt_mapping);

    let file_name = select_prompt_file(mapping, model_hint);
    read_prompt_file(file_name)
        .or_else(|| {
            warn!(
                "Missing system prompt file '{}', falling back to default",
                file_name
            );
            read_prompt_file(&mapping.default_prompt)
        })
        .unwrap_or_else(|| {
            warn!("Falling back to built-in default system prompt");
            DEFAULT_PROMPT_FALLBACK.to_string()
        })
}

fn select_prompt_file<'a>(mapping: &'a PromptMapping, model_hint: Option<&str>) -> &'a str {
    if let Some(hint) = model_hint {
        let hint_lower = hint.to_ascii_lowercase();
        for prompt in &mapping.prompts {
            if prompt
                .model_substrings
                .iter()
                .any(|substr| hint_lower.contains(substr))
            {
                return &prompt.file;
            }
        }

        warn!(
            "No matching system prompt for model hint '{}'; using default prompt '{}'.",
            hint, mapping.default_prompt
        );
    }

    &mapping.default_prompt
}

fn read_prompt_file(file_name: &str) -> Option<String> {
    EmbeddedSystemPrompts::get(file_name).and_then(|file| {
        std::str::from_utf8(file.data.as_ref())
            .map(|content| content.to_string())
            .map_err(|err| warn!("System prompt '{}' is not valid UTF-8: {}", file_name, err))
            .ok()
    })
}

fn load_prompt_mapping() -> PromptMapping {
    match EmbeddedSystemPrompts::get("mapping.json") {
        Some(file) => match serde_json::from_slice::<RawPromptMapping>(file.data.as_ref()) {
            Ok(raw_mapping) => prepare_prompt_mapping(raw_mapping),
            Err(err) => {
                warn!("Failed to deserialize system prompt mapping: {err}; using defaults");
                prepare_prompt_mapping(RawPromptMapping {
                    default_prompt: default_prompt_name(),
                    prompts: Vec::new(),
                })
            }
        },
        None => {
            warn!("System prompt mapping file 'mapping.json' not found; using defaults");
            prepare_prompt_mapping(RawPromptMapping {
                default_prompt: default_prompt_name(),
                prompts: Vec::new(),
            })
        }
    }
}

fn prepare_prompt_mapping(raw: RawPromptMapping) -> PromptMapping {
    let default_prompt = raw.default_prompt;
    let prompts = raw
        .prompts
        .into_iter()
        .map(|raw_prompt| ModelPrompt {
            file: raw_prompt.file,
            model_substrings: raw_prompt
                .model_substrings
                .into_iter()
                .map(|substr| substr.to_ascii_lowercase())
                .collect(),
        })
        .collect();

    PromptMapping {
        default_prompt,
        prompts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_message_generation() {
        // Test Native mode
        let native_msg = generate_system_message(ToolSyntax::Native, ToolScope::Agent, None);
        assert!(!native_msg.contains("{{tools}}"));
        assert!(!native_msg.contains("{{syntax}}"));
        assert!(!native_msg.contains("<tool:"));
        assert!(!native_msg.contains("^^^"));
        assert!(!native_msg.contains("TOOL USE"));

        // Test XML mode
        let xml_msg = generate_system_message(ToolSyntax::Xml, ToolScope::Agent, None);
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

        // Test Caret mode
        let caret_msg = generate_system_message(ToolSyntax::Caret, ToolScope::Agent, None);
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

    #[test]
    fn test_select_prompt_prefers_matching_prompt() {
        let mapping = PromptMapping {
            default_prompt: "default.md".to_string(),
            prompts: vec![ModelPrompt {
                file: "claude.md".to_string(),
                model_substrings: vec!["claude".to_string()],
            }],
        };

        let selected = select_prompt_file(&mapping, Some("Anthropic/Claude-Sonnet-4"));
        assert_eq!(selected, "claude.md");

        let fallback = select_prompt_file(&mapping, Some("gpt-4o"));
        assert_eq!(fallback, "default.md");

        let none_hint = select_prompt_file(&mapping, None);
        assert_eq!(none_hint, "default.md");
    }
}
