pub mod content;
pub mod file_updater;
mod writer;

pub mod encoding;

#[allow(unused_imports)]
pub use command_executor::{
    build_format_command, shell_quote_path, CommandExecutor, CommandOutput, DefaultCommandExecutor,
    StreamingCallback,
};
pub use file_updater::{apply_replacements_normalized, FileUpdaterError};
#[cfg(test)]
pub use writer::MockWriter;
pub use writer::{MessageWriter, StdoutWriter};
