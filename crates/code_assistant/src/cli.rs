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
