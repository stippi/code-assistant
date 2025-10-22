use crate::types::ToolSyntax;
use clap::{Parser, Subcommand};
use llm::provider_config::ConfigurationSystem;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum Mode {
    /// Run as MCP server
    Server {
        /// Enable verbose logging
        #[arg(short, long)]
        verbose: bool,
    },

    /// Run as ACP (Agent Client Protocol) agent
    Acp {
        /// Enable verbose logging
        #[arg(short, long)]
        verbose: bool,

        /// Path to the code directory to analyze
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Model to use from models.json configuration
        #[arg(short = 'm', long)]
        model: Option<String>,

        /// Tool invocation syntax ('native' = tools via API, 'xml' and 'caret' = custom system message)
        #[arg(long, default_value = "native")]
        tool_syntax: ToolSyntax,

        /// Use the legacy diff format for file editing (enables replace_in_file tool instead of edit)
        #[arg(long)]
        use_diff_format: bool,
    },
}

/// Define the application arguments
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
    #[command(subcommand)]
    pub mode: Option<Mode>,

    /// Path to the code directory to analyze
    #[arg(long, default_value = ".")]
    pub path: PathBuf,

    /// Task to perform on the codebase (required in terminal mode, optional with --ui)
    #[arg(short, long)]
    pub task: Option<String>,

    /// Start with GUI interface
    #[arg(long)]
    pub ui: bool,

    /// Continue from previous state
    #[arg(long)]
    pub continue_task: bool,

    /// Enable verbose logging (use multiple times for more verbosity)
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Model to use from models.json configuration
    #[arg(short = 'm', long)]
    pub model: Option<String>,

    /// List available models and exit
    #[arg(long)]
    pub list_models: bool,

    /// List available providers and exit
    #[arg(long)]
    pub list_providers: bool,

    /// Tool invocation syntax ('native' = tools via API, 'xml' and 'caret' = custom system message)
    #[arg(long, default_value = "native")]
    pub tool_syntax: ToolSyntax,

    /// Record API responses to a file (only supported for Anthropic provider currently)
    #[arg(long)]
    pub record: Option<PathBuf>,

    /// Play back a recorded session from a file
    #[arg(long)]
    pub playback: Option<PathBuf>,

    /// Fast playback mode - ignore chunk timing when playing recordings
    #[arg(long)]
    pub fast_playback: bool,

    /// Use the legacy diff format for file editing (enables replace_in_file tool instead of edit)
    #[arg(long)]
    pub use_diff_format: bool,
}

impl Args {
    pub fn parse() -> Self {
        <Args as Parser>::parse()
    }

    /// Resolve a model name, ensuring it exists in the configuration and providing a fallback.
    pub fn resolve_model_name(model: Option<String>) -> anyhow::Result<String> {
        let config = ConfigurationSystem::load()?;

        if let Some(model) = model {
            let trimmed = model.trim();
            if trimmed.is_empty() {
                anyhow::bail!(
                    "Model name cannot be empty. Use --list-models to see available models."
                );
            }

            if config.get_model(trimmed).is_some() {
                return Ok(trimmed.to_string());
            }

            anyhow::bail!(
                "Model '{trimmed}' not found in configuration. Use --list-models to see available models.",
            );
        }

        let mut models = config.list_models();

        if models.is_empty() {
            anyhow::bail!(
                "No models configured. Please:\n\
                1. Copy models.example.json to models.json\n\
                2. Copy providers.example.json to providers.json\n\
                3. Configure your API keys\n\
                4. Specify a model with --model <name> or use --list-models to see available options"
            );
        }

        // Look for common model names as defaults
        let preferred_defaults = ["Claude Sonnet 4.5", "GPT-5", "Claude Opus 4", "GPT-4.1"];
        for default in &preferred_defaults {
            if models.iter().any(|entry| entry == default) {
                return Ok(default.to_string());
            }
        }

        models.sort();
        Ok(models
            .first()
            .expect("models vector is non-empty due to earlier check")
            .clone())
    }

    /// Handle model and provider listing commands
    pub fn handle_list_commands(&self) -> anyhow::Result<bool> {
        if self.list_models || self.list_providers {
            let config = ConfigurationSystem::load()?;

            if self.list_models {
                println!("Available models:");
                let mut models: Vec<_> = config.list_models();
                models.sort();
                for model in models {
                    if let Ok((model_config, provider_config)) =
                        config.get_model_with_provider(&model)
                    {
                        println!(
                            "  {} (provider: {}, id: {})",
                            model, provider_config.label, model_config.id
                        );
                    }
                }
            }

            if self.list_providers {
                println!("Available providers:");
                let mut providers: Vec<_> = config.list_providers();
                providers.sort();
                for provider_id in providers {
                    if let Some(provider) = config.get_provider(&provider_id) {
                        println!("  {} - {}", provider_id, provider.label);
                    }
                }
            }

            return Ok(true); // Indicates that we handled a list command and should exit
        }

        Ok(false) // No list command was handled
    }

    /// Get the model name, with fallback to a default if none specified
    pub fn get_model_name(&self) -> anyhow::Result<String> {
        Self::resolve_model_name(self.model.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_default_args_parsing() {
        // Test that defaults parse correctly
        let args = Args::try_parse_from(["test"]).expect("Failed to parse default args");

        assert_eq!(args.path, std::path::PathBuf::from("."));
        assert_eq!(args.verbose, 0);
        assert!(!args.ui);
        assert!(!args.continue_task);
        assert!(!args.fast_playback);
        assert!(!args.use_diff_format);
        assert!(!args.list_models);
        assert!(!args.list_providers);

        // Check tool syntax default
        matches!(args.tool_syntax, ToolSyntax::Native);
    }

    #[test]
    fn test_verbose_flag_counting() {
        let args = Args::try_parse_from(["test", "-vv"]).expect("Failed to parse verbose args");
        assert_eq!(args.verbose, 2);

        let args =
            Args::try_parse_from(["test", "-v", "-v", "-v"]).expect("Failed to parse verbose args");
        assert_eq!(args.verbose, 3);
    }

    #[test]
    fn test_server_mode() {
        let args = Args::try_parse_from(["test", "server", "--verbose"])
            .expect("Failed to parse server args");

        match args.mode {
            Some(Mode::Server { verbose }) => assert!(verbose),
            _ => panic!("Expected server mode"),
        }
    }

    #[test]
    fn test_acp_mode() {
        let args =
            Args::try_parse_from(["test", "acp", "--verbose"]).expect("Failed to parse acp args");

        match args.mode {
            Some(Mode::Acp { verbose, .. }) => assert!(verbose),
            _ => panic!("Expected acp mode"),
        }
    }
}
