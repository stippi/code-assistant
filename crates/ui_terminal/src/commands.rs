use anyhow::Result;
use llm::provider_config::ConfigurationSystem;

/// Static descriptor for a slash command, used for autocomplete and help display.
pub struct SlashCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
}

/// All registered slash commands.
///
/// This slice is the single source of truth for command discovery. Every entry here
/// corresponds to a match arm in `CommandProcessor::process_command`. Keeping them
/// in sync is intentional: adding a command requires updating both places.
pub fn all_commands() -> &'static [SlashCommand] {
    &[
        SlashCommand {
            name: "help",
            aliases: &["h"],
            description: "Show available commands",
        },
        SlashCommand {
            name: "model",
            aliases: &["m"],
            description: "List available models or switch to one: /model <name>",
        },
        SlashCommand {
            name: "provider",
            aliases: &["p"],
            description: "List available LLM providers",
        },
        SlashCommand {
            name: "current",
            aliases: &["c"],
            description: "Show the currently active model",
        },
        SlashCommand {
            name: "plan",
            aliases: &[],
            description: "Toggle the plan view panel",
        },
        SlashCommand {
            name: "clear",
            aliases: &[],
            description: "Clear the conversation context",
        },
        SlashCommand {
            name: "compact",
            aliases: &[],
            description: "Summarize and compact the conversation context",
        },
    ]
}

/// Result of processing a slash command
#[derive(Debug, Clone)]
pub enum CommandResult {
    /// Continue normal operation
    Continue,
    /// Display help information
    Help(String),
    /// List available models
    ListModels,
    /// List available providers
    ListProviders,
    /// Switch to a specific model
    SwitchModel(String),
    /// Display current model information
    ShowCurrentModel,
    /// Invalid command
    InvalidCommand(String),
    /// Toggle plan rendering mode
    TogglePlan,
}

/// Process slash commands in terminal UI
pub struct CommandProcessor {
    config: ConfigurationSystem,
}

impl CommandProcessor {
    pub fn new() -> Result<Self> {
        let config = ConfigurationSystem::load()?;
        Ok(Self { config })
    }

    /// Process a slash command and return the result
    pub fn process_command(&self, input: &str) -> CommandResult {
        let input = input.trim();

        if !input.starts_with('/') {
            return CommandResult::Continue;
        }

        let parts: Vec<&str> = input[1..].split_whitespace().collect();
        if parts.is_empty() {
            return CommandResult::Help(self.get_help_text());
        }

        match parts[0].to_lowercase().as_str() {
            "help" | "h" => CommandResult::Help(self.get_help_text()),
            "model" | "m" => self.process_model_command(&parts[1..]),
            "provider" | "p" => self.process_provider_command(&parts[1..]),
            "current" | "c" => CommandResult::ShowCurrentModel,
            "plan" => CommandResult::TogglePlan,
            _ => CommandResult::InvalidCommand(format!("Unknown command: /{}", parts[0])),
        }
    }

    fn process_model_command(&self, args: &[&str]) -> CommandResult {
        if args.is_empty() {
            return CommandResult::ListModels;
        }

        let model_name = args.join(" ");

        // Check if the model exists
        if self.config.models.contains_key(&model_name) {
            CommandResult::SwitchModel(model_name)
        } else {
            CommandResult::InvalidCommand(format!(
                "Model '{model_name}' not found. Use '/model' to list available models.",
            ))
        }
    }

    fn process_provider_command(&self, args: &[&str]) -> CommandResult {
        if args.is_empty() {
            return CommandResult::ListProviders;
        }

        CommandResult::InvalidCommand(
            "Provider switching not supported. Use '/model <name>' to switch models.".to_string(),
        )
    }

    fn get_help_text(&self) -> String {
        let mut text = String::from("Available commands:\n");
        for cmd in all_commands() {
            if cmd.aliases.is_empty() {
                text.push_str(&format!("  /{:<16} {}\n", cmd.name, cmd.description));
            } else {
                let alias_list = cmd
                    .aliases
                    .iter()
                    .map(|a| format!("/{a}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                text.push_str(&format!(
                    "  /{}, {:<12} {}\n",
                    cmd.name, alias_list, cmd.description
                ));
            }
        }
        text.push_str("\nExamples:\n  /model Claude Sonnet 4.5\n  /model GPT-5");
        text
    }

    /// Get formatted list of available models
    pub fn get_models_list(&self) -> String {
        let mut models: Vec<_> = self.config.models.keys().cloned().collect();
        models.sort();

        let mut output = String::from("Available models:\n");
        for model_name in models {
            if let Ok((_model_config, provider_config)) =
                self.config.get_model_with_provider(&model_name)
            {
                output.push_str(&format!("  {} ({})\n", model_name, provider_config.label));
            }
        }
        output
    }

    /// Get formatted list of available providers
    pub fn get_providers_list(&self) -> String {
        let mut providers: Vec<_> = self.config.providers.keys().cloned().collect();
        providers.sort();

        let mut output = String::from("Available providers:\n");
        for provider_id in providers {
            if let Some(provider) = self.config.providers.get(&provider_id) {
                output.push_str(&format!("  {} ({})\n", provider_id, provider.label));
            }
        }
        output
    }
}
