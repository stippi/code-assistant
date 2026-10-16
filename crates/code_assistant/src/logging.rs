use std::fs::OpenOptions;
use std::io;
use tracing_subscriber::fmt::SubscriberBuilder;
use tracing_subscriber::fmt::writer::MakeWriterExt;

pub fn setup_logging(verbose_level: u8, to_stdout: bool) {
    setup_logging_with_file(verbose_level, to_stdout, None);
}

/// Once a log file exceeds this size at startup, it is moved aside to
/// `<name>.old` (replacing the previous one) and a fresh file is started.
const MAX_LOG_FILE_SIZE: u64 = 20 * 1024 * 1024;

/// `<cache_dir>/code-assistant/<file_name>`, with the directory created and
/// an oversized file rotated.
fn log_file_path(file_name: &str) -> std::path::PathBuf {
    let path = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("code-assistant")
        .join(file_name);

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_LOG_FILE_SIZE) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    path
}

pub fn setup_logging_for_terminal_ui(verbose_level: u8) {
    // For terminal UI, log to a file to prevent interference with the UI
    setup_logging_with_file(verbose_level, false, Some(log_file_path("terminal-ui.log")));
}

/// The GPUI app usually runs as a bundle whose stderr nobody sees, so it logs
/// to `<cache_dir>/code-assistant/app.log` as well as to stderr.
pub fn setup_logging_for_gpui(verbose_level: u8) {
    let path = log_file_path("app.log");
    let file = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("Warning: Could not open log file {path:?} ({e}), logging to stderr only");
            setup_logging(verbose_level, false);
            return;
        }
    };
    let file = std::sync::Mutex::new(file);
    tracing_subscriber::fmt()
        .with_env_filter(env_filter(verbose_level))
        .with_target(false)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true)
        .with_level(true)
        // No color escapes in the file.
        .with_ansi(false)
        .with_writer(std::io::stderr.and(file))
        .init();
}

fn env_filter(verbose_level: u8) -> tracing_subscriber::EnvFilter {
    if std::env::var("RUST_LOG").is_ok() {
        // Use RUST_LOG if set
        tracing_subscriber::EnvFilter::from_default_env()
    } else {
        // Map verbosity count to filters
        let filter_str = match verbose_level {
            0 => "warn,code_assistant=info,agent_core=info,llm=info,web=info",
            1 => "info,code_assistant=debug,agent_core=debug,llm=debug,web=debug",
            _ => "debug,code_assistant=trace,agent_core=trace,llm=trace,web=trace",
        };
        tracing_subscriber::EnvFilter::new(filter_str)
    }
}

fn setup_logging_with_file(
    verbose_level: u8,
    to_stdout: bool,
    log_file: Option<std::path::PathBuf>,
) {
    let filter = env_filter(verbose_level);

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true)
        .with_level(true);

    // Choose output destination
    if let Some(log_file_path) = log_file {
        // Log to file (for terminal UI)
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file_path)
            .unwrap_or_else(|_| {
                eprintln!(
                    "Warning: Could not open log file {log_file_path:?}, falling back to stderr"
                );
                std::fs::File::create("/dev/null").unwrap_or_else(|_| {
                    // On Windows, use NUL device
                    std::fs::File::create("NUL").unwrap_or_else(|_| {
                        panic!("Could not create log file or null device");
                    })
                })
            });

        subscriber
            .with_writer(move || {
                Box::new(file.try_clone().expect("Failed to clone file handle"))
                    as Box<dyn io::Write + Send>
            })
            .init();
    } else {
        // For server mode, write only to stderr to keep stdout clean for JSON-RPC
        let subscriber: SubscriberBuilder<_, _, _, fn() -> Box<dyn io::Write + Send>> = if to_stdout
        {
            subscriber.with_writer(|| Box::new(std::io::stdout()) as Box<dyn io::Write + Send>)
        } else {
            subscriber.with_writer(|| Box::new(std::io::stderr()) as Box<dyn io::Write + Send>)
        };

        subscriber.init();
    }
}
