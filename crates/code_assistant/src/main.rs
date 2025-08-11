mod agent;
mod app;
mod cli;
mod config;
mod explorer;
mod logging;
mod mcp;
mod persistence;
mod session;
mod tools;
mod types;
mod ui;
mod utils;

#[cfg(test)]
mod tests;

use crate::cli::{Args, Mode};
use crate::logging::setup_logging;
use anyhow::Result;























#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    match args.mode {
        Some(Mode::Server { verbose }) => app::server::run(verbose).await,
        None => {
            // Use stderr for both terminal and GPUI modes to keep stdout clean
            setup_logging(args.verbose, false);

            // Ensure the path exists and is a directory
            if !args.path.is_dir() {
                anyhow::bail!("Path '{}' is not a directory", args.path.display());
            }

            let config = app::AgentRunConfig {
                path: args.path,
                task: args.task,
                continue_task: args.continue_task,
                provider: args.provider,
                model: args.model,
                base_url: args.base_url,
                aicore_config: args.aicore_config,
                num_ctx: args.num_ctx,
                tool_syntax: args.tool_syntax,
                use_diff_format: args.use_diff_format,
                record: args.record,
                playback: args.playback,
                fast_playback: args.fast_playback,
            };

            if args.ui {
                app::gpui::run(config)
            } else {
                app::terminal::run(config).await
            }
        }
    }
}
