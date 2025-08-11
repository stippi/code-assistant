use std::io;
use tracing_subscriber::fmt::SubscriberBuilder;

pub fn setup_logging(verbose_level: u8, to_stdout: bool) {
    let filter = if std::env::var("RUST_LOG").is_ok() {
        // Use RUST_LOG if set
        tracing_subscriber::EnvFilter::from_default_env()
    } else {
        // Map verbosity count to filters
        let filter_str = match verbose_level {
            0 => "code_assistant=info,llm=info,web=info,warn",
            1 => "code_assistant=debug,llm=debug,web=debug,info",
            _ => "code_assistant=trace,llm=trace,web=trace,debug",
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

    // For server mode, write only to stderr to keep stdout clean for JSON-RPC
    let subscriber: SubscriberBuilder<_, _, _, fn() -> Box<dyn io::Write + Send>> = if to_stdout {
        subscriber.with_writer(|| Box::new(std::io::stdout()) as Box<dyn io::Write + Send>)
    } else {
        subscriber.with_writer(|| Box::new(std::io::stderr()) as Box<dyn io::Write + Send>)
    };

    subscriber.init();
}
