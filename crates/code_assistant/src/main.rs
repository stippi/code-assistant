mod acp;
mod agent;
mod app;
mod cli;
mod config;
mod logging;
mod mcp;
mod permissions;
mod persistence;
mod session;
mod settings;
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

    // Handle list commands first
    if args.handle_list_commands()? {
        return Ok(());
    }

    match args.mode {
        Some(Mode::Server { verbose }) => app::server::run(verbose).await,
        Some(Mode::Acp {
            verbose,
            path,
            model,
            tool_syntax,
            use_diff_format,
            sandbox_mode,
            sandbox_network,
        }) => {
            // Ensure the path exists and is a directory
            if !path.is_dir() {
                anyhow::bail!("Path '{}' is not a directory", path.display());
            }

            let model_name = Args::resolve_model_name(model)?;

            let config = app::AgentRunConfig {
                path,
                task: None,
                continue_task: false,
                model: model_name.clone(),
                tool_syntax,
                use_diff_format,
                record: None,
                playback: None,
                fast_playback: false,
                sandbox_policy: sandbox_mode.to_policy(sandbox_network),
            };

            app::acp::run(verbose, config).await
        }
        None => {
            if args.ui {
                // GPUI mode - use stderr to keep stdout clean
                setup_logging(args.verbose, false);
            } else {
                // Terminal UI mode - log to file to prevent UI interference
                logging::setup_logging_for_terminal_ui(args.verbose);
            }

            // Ensure the path exists and is a directory
            if !args.path.is_dir() {
                anyhow::bail!("Path '{}' is not a directory", args.path.display());
            }

            let model_name = args.get_model_name()?;
            let sandbox_policy = args.sandbox_policy();

            let config = app::AgentRunConfig {
                path: args.path,
                task: args.task,
                continue_task: args.continue_task,
                model: model_name,
                tool_syntax: args.tool_syntax,
                use_diff_format: args.use_diff_format,
                record: args.record,
                playback: args.playback,
                fast_playback: args.fast_playback,
                sandbox_policy,
            };

            if args.ui {
                app::gpui::run(config)
            } else {
                app::terminal::run(config).await
            }
        }
    }
}
