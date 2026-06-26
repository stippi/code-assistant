mod app;
mod cli;
mod codex_commands;
mod logging;

// The domain layer lives in `code_assistant_core`; re-exported under the
// historical module paths so call sites keep using `crate::session::…` etc.
#[allow(unused_imports)]
pub(crate) use code_assistant_core::{
    agent, config, config_dir, persistence, plugins, session, skills, tool_dialects, tools, types,
    ui, utils,
};

use crate::cli::{Args, Mode};
use crate::logging::setup_logging;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Apply config-dir override before anything loads config files
    if let Some(ref dir) = args.config_dir {
        config_dir::apply_override(dir);
    }

    // Handle list commands first
    if args.handle_list_commands()? {
        return Ok(());
    }

    // Extract bundled system skills into <config_dir>/skills/.system. Idempotent
    // (fingerprint-gated); best-effort so a failure never blocks startup.
    if let Err(e) = skills::install_system_skills() {
        tracing::warn!("Failed to install bundled system skills: {e:#}");
    }

    match args.mode {
        Some(Mode::CodexLogin) => {
            setup_logging(1, true);
            return codex_commands::run_codex_login().await;
        }
        Some(Mode::CodexLogout) => {
            return codex_commands::run_codex_logout();
        }
        Some(Mode::CodexStatus) => {
            return codex_commands::run_codex_status();
        }
        Some(Mode::Server { verbose }) => {
            #[cfg(feature = "mcp-server")]
            {
                app::server::run(verbose).await
            }
            #[cfg(not(feature = "mcp-server"))]
            {
                let _ = verbose;
                anyhow::bail!(
                    "This binary was built without the MCP server \
                     (feature `mcp-server`)"
                )
            }
        }
        Some(Mode::Acp {
            verbose,
            path,
            model,
            tool_syntax,
            use_diff_format,
            sandbox_mode,
            sandbox_network,
        }) => {
            #[cfg(feature = "acp-frontend")]
            {
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
            #[cfg(not(feature = "acp-frontend"))]
            {
                let _ = (
                    verbose,
                    path,
                    model,
                    tool_syntax,
                    use_diff_format,
                    sandbox_mode,
                    sandbox_network,
                );
                anyhow::bail!(
                    "This binary was built without the ACP frontend \
                     (feature `acp-frontend`)"
                )
            }
        }
        None => {
            // Determine whether to launch the GPUI frontend. The graphical
            // interface is the default when compiled in; otherwise we fall
            // back to the terminal frontend so the binary stays useful in
            // headless builds (e.g. `--no-default-features`).
            #[cfg(feature = "gpui-frontend")]
            let use_gpui = !args.tui;
            #[cfg(not(feature = "gpui-frontend"))]
            let use_gpui = false;

            if use_gpui {
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

            // In GUI mode, allow starting without a valid model config
            // (the settings screen will guide the user through setup).
            let model_name = if use_gpui {
                args.get_model_name().unwrap_or_default()
            } else {
                args.get_model_name()?
            };
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

            if use_gpui {
                #[cfg(feature = "gpui-frontend")]
                {
                    app::gpui::run(config)
                }
                #[cfg(not(feature = "gpui-frontend"))]
                {
                    let _ = config;
                    unreachable!("use_gpui is only true when the gpui-frontend feature is enabled")
                }
            } else {
                #[cfg(feature = "terminal-frontend")]
                {
                    app::terminal::run(config).await
                }
                #[cfg(not(feature = "terminal-frontend"))]
                {
                    let _ = config;
                    anyhow::bail!(
                        "This binary was built without the terminal frontend \
                         (feature `terminal-frontend`)"
                    )
                }
            }
        }
    }
}
