mod command;
pub mod content;
mod file_updater;
mod writer;

pub mod encoding;

#[allow(unused_imports)]
pub use command::{CommandExecutor, CommandOutput, DefaultCommandExecutor};
pub use file_updater::{apply_replacements_normalized, FileUpdaterError};
#[cfg(test)]
pub use writer::MockWriter;
pub use writer::{MessageWriter, StdoutWriter};
