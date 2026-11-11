//! System message generation functionality

use crate::tools::core::ToolScope;
use agent_core::ToolDialect;
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

/// Generate a system message for the given tool dialect, scope, and optional model hint
pub fn generate_system_message(
    dialect: &dyn ToolDialect,
    scope: ToolScope,
    model_hint: Option<&str>,
    registry: &crate::tools::core::ToolRegistry,
) -> String {
    let mut base = load_base_prompt(model_hint).replace("{{environment}}", &environment_section());

    if dialect.uses_native_tools() {
        // For native mode, replace the entire placeholder section with an empty line
        base = base.replace("{{syntax}}\n\n{{tools}}\n\n", "\n");
        base
    } else {
        // For text dialects, ask the dialect for its documentation sections
        let tool_use_header = "TOOL USE\n\n";

        // Replace syntax documentation with tools introduction + syntax doc
        if let Some(syntax_doc) = dialect.render_format_section_for_prompt() {
            let syntax_content = format!("{TOOLS_INTRODUCTION}{syntax_doc}");
            base = base.replace("{{syntax}}", &syntax_content);
        } else {
            base = base.replace("{{syntax}}", TOOLS_INTRODUCTION);
        }

        // Replace tools documentation with header + tools doc
        if let Some(tools_doc) = dialect.render_tool_section_for_prompt(registry, scope.tag()) {
            let tools_content = format!("{tool_use_header}{tools_doc}");
            base = base.replace("{{tools}}", &tools_content);
        } else {
            base = base.replace("{{tools}}", tool_use_header);
        }

        base
    }
}

/// The `{{environment}}` section: where commands run. It must not change
/// during a session (prompt-cache prefix), so it holds nothing like a date.
fn render_environment(os: &str, arch: &str, shell: &command_executor::Shell) -> String {
    let os_name = match os {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    };
    let invocation = [shell.program.as_str()]
        .into_iter()
        .chain(shell.args.iter().copied())
        .chain(["<command>"])
        .collect::<Vec<_>>()
        .join(" ");
    let mut section = format!(
        "# Environment\n\n\
         - Operating system: {os_name} ({arch})\n\
         - Shell: `execute_command` runs each command as `{invocation}`.\n"
    );
    if os == "windows" {
        section.push_str(
            "- Commands therefore run in cmd.exe, not in bash or PowerShell: use cmd.exe syntax \
             (`&&` to chain, `%VAR%`, double quotes) and Windows commands. Unix tools such as \
             `ls`, `grep`, `cat` or `rm` may be missing; call `powershell -NoProfile -Command \"...\"` \
             when you need more than cmd.exe offers.\n",
        );
    }
    // The placeholder sits on its own line; the template supplies spacing.
    section.truncate(section.trim_end().len());
    section
}

fn environment_section() -> String {
    render_environment(
        std::env::consts::OS,
        std::env::consts::ARCH,
        &command_executor::shell(),
    )
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
                warn!(
                    "Using system prompt for model hint '{}': '{}'.",
                    hint, &prompt.file
                );
                return &prompt.file;
            }
        }

        warn!(
            "No matching system prompt for model hint '{}'; using default prompt '{}'.",
            hint, mapping.default_prompt
        );
    } else {
        warn!(
            "No model hint provided; using default prompt '{}'.",
            mapping.default_prompt
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
        use crate::types::ToolSyntax;

        // Test Native mode
        let native_msg = generate_system_message(
            &*crate::tool_dialects::dialect_for(ToolSyntax::Native),
            ToolScope::Agent,
            None,
            &crate::tools::test_registry(),
        );
        assert!(!native_msg.contains("{{tools}}"));
        assert!(!native_msg.contains("{{syntax}}"));
        assert!(!native_msg.contains("<tool:"));
        assert!(!native_msg.contains("^^^"));
        assert!(!native_msg.contains("TOOL USE"));

        // Test XML mode
        let xml_msg = generate_system_message(
            &*crate::tool_dialects::dialect_for(ToolSyntax::Xml),
            ToolScope::Agent,
            None,
            &crate::tools::test_registry(),
        );
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
        let caret_msg = generate_system_message(
            &*crate::tool_dialects::dialect_for(ToolSyntax::Caret),
            ToolScope::Agent,
            None,
            &crate::tools::test_registry(),
        );
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
    fn every_prompt_has_an_environment_placeholder() {
        let prompts: Vec<_> = EmbeddedSystemPrompts::iter()
            .filter(|name| name.ends_with(".md"))
            .collect();
        assert!(!prompts.is_empty());
        for name in prompts {
            let prompt = read_prompt_file(&name).unwrap();
            assert!(
                prompt.contains("{{environment}}"),
                "{name} lacks the {{{{environment}}}} placeholder"
            );
        }
    }

    #[test]
    fn system_message_describes_the_environment() {
        use crate::types::ToolSyntax;

        for syntax in [ToolSyntax::Native, ToolSyntax::Xml, ToolSyntax::Caret] {
            for hint in [None, Some("claude-sonnet"), Some("gpt-5-codex")] {
                let message = generate_system_message(
                    &*crate::tool_dialects::dialect_for(syntax),
                    ToolScope::Agent,
                    hint,
                    &crate::tools::test_registry(),
                );
                assert!(!message.contains("{{environment}}"), "{syntax:?} {hint:?}");
                assert!(message.contains("# Environment"), "{syntax:?} {hint:?}");
                assert!(
                    message.contains(&command_executor::shell().program),
                    "{syntax:?} {hint:?}"
                );
            }
        }
    }

    #[test]
    fn environment_names_the_unix_shell_invocation() {
        let shell = command_executor::Shell {
            program: "/bin/zsh".into(),
            args: &["-l", "-c"],
        };
        let section = render_environment("macos", "aarch64", &shell);
        assert!(section.contains("macOS (aarch64)"), "{section}");
        assert!(section.contains("`/bin/zsh -l -c <command>`"), "{section}");
        assert!(!section.contains("cmd.exe"), "{section}");
    }

    #[test]
    fn environment_warns_about_cmd_on_windows() {
        let shell = command_executor::Shell {
            program: "cmd".into(),
            args: &["/C"],
        };
        let section = render_environment("windows", "x86_64", &shell);
        assert!(section.contains("Windows (x86_64)"), "{section}");
        assert!(section.contains("`cmd /C <command>`"), "{section}");
        assert!(section.contains("cmd.exe syntax"), "{section}");
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
