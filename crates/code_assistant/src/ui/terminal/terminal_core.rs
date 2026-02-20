use anyhow::Result;
use ratatui::{backend::Backend, text::Line, Terminal};

use super::history_insert;

pub struct TerminalCore<B: Backend> {
    terminal_factory: Box<dyn Fn() -> Result<Terminal<B>> + Send + Sync>,
}

impl<B: Backend> TerminalCore<B> {
    pub fn with_factory<F>(factory: F) -> Self
    where
        F: Fn() -> Result<Terminal<B>> + Send + Sync + 'static,
    {
        Self {
            terminal_factory: Box::new(factory),
        }
    }

    pub fn create_terminal(&self) -> Result<Terminal<B>> {
        (self.terminal_factory)()
    }

    pub fn setup_terminal(&self) -> Result<()> {
        ratatui::crossterm::terminal::enable_raw_mode()?;
        Ok(())
    }

    pub fn cleanup_terminal(&self) -> Result<()> {
        ratatui::crossterm::terminal::disable_raw_mode()?;
        Ok(())
    }

    pub fn insert_history_lines(
        &self,
        terminal: &mut Terminal<B>,
        lines: &[Line<'static>],
    ) -> Result<()> {
        history_insert::insert_history_lines(terminal, lines)?;
        Ok(())
    }
}
