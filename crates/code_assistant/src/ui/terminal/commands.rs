use anyhow::Result;
use llm::provider_config::ConfigurationSystem;

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
        concat!(
            "Available commands:\n",
            "/help, /h          - Show this help\n",
            "/model, /m         - List available models\n",
            "/model <name>      - Switch to model\n",
            "/provider, /p      - List available providers\n",
            "/current, /c       - Show current model\n",
            "\n",
            "Examples:\n",
            "/model Claude Sonnet 4.5\n",
            "/model GPT-5",
        )
        .to_string()
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
