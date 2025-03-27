mod command;
mod file_updater;

pub mod encoding;

#[allow(unused_imports)]
pub use command::{CommandExecutor, CommandOutput, DefaultCommandExecutor};
pub use file_updater::{apply_replacements_normalized, FileUpdaterError};
