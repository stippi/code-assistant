use std::fs::OpenOptions;
use std::io;
use tracing_subscriber::fmt::SubscriberBuilder;

pub fn setup_logging(verbose_level: u8, to_stdout: bool) {
    setup_logging_with_file(verbose_level, to_stdout, None);
}

pub fn setup_logging_for_terminal_ui(verbose_level: u8) {
    // For terminal UI, log to a file to prevent interference with the UI
    let log_file_path = dirs::cache_dir()
        .unwrap_or_else(|| std::env::temp_dir())
        .join("code-assistant")
        .join("terminal-ui.log");

    // Create directory if it doesn't exist
    if let Some(parent) = log_file_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    setup_logging_with_file(verbose_level, false, Some(log_file_path));
}

fn setup_logging_with_file(
    verbose_level: u8,
    to_stdout: bool,
    log_file: Option<std::path::PathBuf>,
) {
    let filter = if std::env::var("RUST_LOG").is_ok() {
        // Use RUST_LOG if set
        tracing_subscriber::EnvFilter::from_default_env()
    } else {
        // Map verbosity count to filters
        let filter_str = match verbose_level {
            0 => "warn,code_assistant=info,llm=info,web=info",
            1 => "info,code_assistant=debug,llm=debug,web=debug",
            _ => "debug,code_assistant=trace,llm=trace,web=trace",
        };
        tracing_subscriber::EnvFilter::new(filter_str)
    };

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
                    "Warning: Could not open log file {:?}, falling back to stderr",
                    log_file_path
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
