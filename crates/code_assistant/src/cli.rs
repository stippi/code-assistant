use crate::types::ToolSyntax;
use crate::version;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use llm::provider_config::ConfigurationSystem;
use sandbox::SandboxPolicy;
use std::path::PathBuf;

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum SandboxModeArg {
    #[value(name = "danger-full-access")]
    DangerFullAccess,
    #[value(name = "read-only")]
    ReadOnly,
    #[value(name = "workspace-write")]
    WorkspaceWrite,
}

/// What `code-assistant plugin` does.
#[derive(Subcommand, Debug)]
pub enum PluginCommand {
    /// Install a plugin from a git repository or a local folder
    Add {
        /// A git URL (`https://…`, `git@…`, `file://…`), GitHub's
        /// `owner/repo`, either followed by `#branch-or-tag`, or a folder
        source: String,
    },
    /// List the installed plugins
    List,
    /// Fetch a plugin (or all git plugins) again from its repository
    Update {
        /// The plugin's name; all plugins when omitted
        name: Option<String>,
    },
    /// Uninstall a plugin
    Remove { name: String },
    /// Switch a plugin on
    Enable { name: String },
    /// Switch a plugin off
    Disable { name: String },
}

#[derive(Subcommand, Debug)]
pub enum Mode {
    /// Run as MCP server
    Server {
        /// Enable verbose logging
        #[arg(short, long)]
        verbose: bool,
    },

    /// Log in with your ChatGPT subscription (opens browser for OAuth)
    CodexLogin,

    /// Log out from ChatGPT subscription (removes stored tokens)
    CodexLogout,

    /// Show ChatGPT subscription auth status
    CodexStatus,

    /// Log in to an HTTP MCP server that requires OAuth (opens browser)
    McpLogin {
        /// Name of the server as configured in mcp-servers.json or in the
        /// project's trusted .mcp.json
        server: String,

        /// Path to the project whose .mcp.json may define the server
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },

    /// Remove stored OAuth tokens for an HTTP MCP server
    McpLogout {
        /// Name of the server; removes its logins for every URL
        server: String,
    },

    /// Install, list, update or remove plugins
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },

    /// Run as ACP (Agent Client Protocol) agent
    Acp {
        /// Enable verbose logging
        #[arg(short, long)]
        verbose: bool,

        /// Path to the code directory to analyze
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Model to use: its name or key (`provider/model-id`)
        #[arg(short = 'm', long)]
        model: Option<String>,

        /// Tool invocation syntax ('native' = tools via API, 'xml' and 'caret' = custom system message)
        #[arg(long, default_value = "native")]
        tool_syntax: ToolSyntax,

        /// Use the legacy diff format for file editing (enables replace_in_file tool instead of edit)
        #[arg(long)]
        use_diff_format: bool,

        /// Sandbox mode for command execution
        #[arg(long, value_enum, default_value_t = SandboxModeArg::DangerFullAccess)]
        sandbox_mode: SandboxModeArg,

        /// Allow network access when sandbox mode is workspace-write
        #[arg(long, default_value_t = false)]
        sandbox_network: bool,
    },
}

/// Define the application arguments
#[derive(Parser, Debug)]
#[command(about, long_about = None)]
pub struct Args {
    #[command(subcommand)]
    pub mode: Option<Mode>,

    /// Override configuration directory (where providers.json, model-selection.json, etc. live)
    #[arg(long)]
    pub config_dir: Option<PathBuf>,

    /// Path to the code directory to analyze
    #[arg(long, default_value = ".")]
    pub path: PathBuf,

    /// Task to perform on the codebase (required in terminal mode, optional in GUI mode)
    #[arg(short, long)]
    pub task: Option<String>,

    /// Start with terminal interface instead of GUI
    #[arg(long)]
    pub tui: bool,

    /// Continue from previous state
    #[arg(long)]
    pub continue_task: bool,

    /// Enable verbose logging (use multiple times for more verbosity)
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Model to use: its name or key (`provider/model-id`)
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

    /// Sandbox mode for command execution
    #[arg(long, value_enum, default_value_t = SandboxModeArg::DangerFullAccess)]
    pub sandbox_mode: SandboxModeArg,

    /// Allow network access when sandbox mode is workspace-write
    #[arg(long, default_value_t = false)]
    pub sandbox_network: bool,
}

impl Args {
    pub fn parse() -> Self {
        // Build the command with goreleaser-style version strings captured at
        // compile time: `-V` prints the one-line summary, `--version` prints
        // the full commit/build/toolchain block. Leaked to `&'static str`
        // because clap wants an owned static; `parse` runs once at startup.
        let short: &'static str = Box::leak(version::short().into_boxed_str());
        let long: &'static str = Box::leak(version::long().into_boxed_str());
        let command = <Args as CommandFactory>::command()
            .version(short)
            .long_version(long);
        let matches = command.get_matches();
        <Args as FromArgMatches>::from_arg_matches(&matches).unwrap_or_else(|err| err.exit())
    }

    /// Resolve a model name, ensuring it exists in the configuration and providing a fallback.
    ///
    /// Resolution order:
    /// 1. Explicit `--model` CLI argument
    /// 2. The configured default model (`default-model.json`)
    /// 3. The first offered model by name
    pub fn resolve_model_name(model: Option<String>) -> anyhow::Result<String> {
        let config = ConfigurationSystem::load()?;

        if let Some(model) = model {
            let trimmed = model.trim();
            if trimmed.is_empty() {
                anyhow::bail!(
                    "Model name cannot be empty. Use --list-models to see available models."
                );
            }

            if let Some(key) = config.resolve_key(trimmed) {
                return Ok(key);
            }

            anyhow::bail!(
                "Model '{trimmed}' not found in configuration. Use --list-models to see available models.",
            );
        }

        let models = config.list_models();

        if models.is_empty() {
            anyhow::bail!(
                "No models configured. Please:\n\
                1. Copy providers.example.json to providers.json and configure your API keys\n\
                2. Choose models in the GPUI settings, or list them in model-selection.json\n\
                3. Specify a model with --model <name> or use --list-models to see available options"
            );
        }

        // The configured default model (`default-model.json`).
        if let Some(default_model) =
            code_assistant_core::session::default_model::configured_default_model()
        {
            return Ok(default_model);
        }

        // Fallback: the first model by name
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
                for key in config.list_models() {
                    if let Ok((_, provider_config)) = config.get_model_with_provider(&key) {
                        println!(
                            "  {} (provider: {}, key: {key})",
                            config.display_name(&key),
                            provider_config.label
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

    pub fn sandbox_policy(&self) -> SandboxPolicy {
        self.sandbox_mode.to_policy(self.sandbox_network)
    }
}

impl SandboxModeArg {
    pub fn to_policy(self, network: bool) -> SandboxPolicy {
        match self {
            SandboxModeArg::DangerFullAccess => SandboxPolicy::DangerFullAccess,
            SandboxModeArg::ReadOnly => SandboxPolicy::ReadOnly,
            SandboxModeArg::WorkspaceWrite => SandboxPolicy::WorkspaceWrite {
                writable_roots: Vec::new(),
                network_access: network,
                exclude_tmpdir_env_var: false,
                exclude_slash_tmp: false,
            },
        }
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
        assert!(!args.tui);
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

    #[test]
    fn test_mcp_login_mode() {
        let args = Args::try_parse_from(["test", "mcp-login", "remote", "--path", "/project"])
            .expect("Failed to parse mcp-login args");

        match args.mode {
            Some(Mode::McpLogin { server, path }) => {
                assert_eq!(server, "remote");
                assert_eq!(path, std::path::PathBuf::from("/project"));
            }
            _ => panic!("Expected mcp-login mode"),
        }
    }

    #[test]
    fn test_plugin_mode() {
        let args = Args::try_parse_from(["test", "plugin", "add", "acme/tools#v1"])
            .expect("Failed to parse plugin add");
        match args.mode {
            Some(Mode::Plugin {
                command: PluginCommand::Add { source },
            }) => assert_eq!(source, "acme/tools#v1"),
            other => panic!("Expected plugin add, got {other:?}"),
        }
        let args = Args::try_parse_from(["test", "plugin", "update"]).unwrap();
        assert!(matches!(
            args.mode,
            Some(Mode::Plugin {
                command: PluginCommand::Update { name: None }
            })
        ));
    }
}
