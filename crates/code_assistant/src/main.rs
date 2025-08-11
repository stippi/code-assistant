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

            if args.ui {
                app::gpui::run(
                    args.path,
                    args.task,
                    args.provider,
                    args.model,
                    args.base_url,
                    args.aicore_config,
                    args.num_ctx,
                    args.tool_syntax,
                    args.use_diff_format,
                    args.record,
                    args.playback,
                    args.fast_playback,
                )
            } else {
                app::terminal::run(
                    args.path,
                    args.task,
                    args.continue_task,
                    args.provider,
                    args.model,
                    args.base_url,
                    args.aicore_config,
                    args.num_ctx,
                    args.tool_syntax,
                    args.use_diff_format,
                    args.record,
                    args.playback,
                    args.fast_playback,
                ).await
            }
        }
    }
}
