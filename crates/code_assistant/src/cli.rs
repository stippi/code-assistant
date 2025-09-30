use crate::types::ToolSyntax;
use clap::{Parser, Subcommand};
use llm::factory::LLMProviderType;
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

    /// LLM provider to use
    #[arg(short = 'p', long, default_value = "anthropic")]
    pub provider: LLMProviderType,

    /// Model name to use (provider-specific)
    #[arg(short = 'm', long)]
    pub model: Option<String>,

    /// API base URL for the LLM provider to use
    #[arg(long)]
    pub base_url: Option<String>,

    /// Path to AI Core configuration file
    #[arg(long)]
    pub aicore_config: Option<PathBuf>,

    /// Context window size (in tokens, only relevant for Ollama)
    #[arg(long, default_value_t = 8192)]
    pub num_ctx: usize,

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
        assert_eq!(args.num_ctx, 8192);
        assert!(!args.ui);
        assert!(!args.continue_task);
        assert!(!args.fast_playback);
        assert!(!args.use_diff_format);

        // Check provider default
        matches!(args.provider, LLMProviderType::Anthropic);

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
            Some(Mode::Acp { verbose }) => assert!(verbose),
            _ => panic!("Expected acp mode"),
        }
    }
}
