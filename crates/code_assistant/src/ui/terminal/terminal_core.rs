use anyhow::Result;
use ratatui::{backend::Backend, text::Line, Terminal};
use std::panic::PanicHookInfo;
use std::sync::Arc;

use super::history_insert;

type PanicHookFn = dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static;
type RawModeOp = dyn Fn() -> Result<()> + Send + Sync + 'static;

#[derive(Clone)]
struct RawModeOps {
    enable: Arc<RawModeOp>,
    disable: Arc<RawModeOp>,
}

impl RawModeOps {
    fn crossterm() -> Self {
        Self {
            enable: Arc::new(|| {
                ratatui::crossterm::terminal::enable_raw_mode()?;
                Ok(())
            }),
            disable: Arc::new(|| {
                ratatui::crossterm::terminal::disable_raw_mode()?;
                Ok(())
            }),
        }
    }
}

struct TerminalRuntimeGuard {
    previous_panic_hook: Arc<PanicHookFn>,
    disable_raw_mode_op: Arc<RawModeOp>,
    raw_mode_enabled: bool,
}

impl TerminalRuntimeGuard {
    fn install(raw_mode_ops: &RawModeOps) -> Result<Self> {
        (raw_mode_ops.enable)()?;

        let previous_hook: Arc<PanicHookFn> = Arc::from(std::panic::take_hook());
        let hook_for_panic = Arc::clone(&previous_hook);
        let disable_for_panic = Arc::clone(&raw_mode_ops.disable);
        std::panic::set_hook(Box::new(move |panic_info| {
            let _ = disable_for_panic();
            hook_for_panic(panic_info);
        }));

        Ok(Self {
            previous_panic_hook: previous_hook,
            disable_raw_mode_op: Arc::clone(&raw_mode_ops.disable),
            raw_mode_enabled: true,
        })
    }

    fn cleanup(&mut self) -> Result<()> {
        self.restore_panic_hook();
        self.disable_raw_mode()
    }

    fn disable_raw_mode(&mut self) -> Result<()> {
        if self.raw_mode_enabled {
            (self.disable_raw_mode_op)()?;
            self.raw_mode_enabled = false;
        }
        Ok(())
    }

    fn restore_panic_hook(&self) {
        let hook_for_restore = Arc::clone(&self.previous_panic_hook);
        std::panic::set_hook(Box::new(move |panic_info| {
            hook_for_restore(panic_info);
        }));
    }
}

impl Drop for TerminalRuntimeGuard {
    fn drop(&mut self) {
        self.restore_panic_hook();
        let _ = self.disable_raw_mode();
    }
}

pub struct TerminalCore<B: Backend> {
    terminal_factory: Box<dyn Fn() -> Result<Terminal<B>> + Send + Sync>,
    raw_mode_ops: RawModeOps,
    runtime_guard: Option<TerminalRuntimeGuard>,
}

impl<B: Backend> TerminalCore<B> {
    pub fn with_factory<F>(factory: F) -> Self
    where
        F: Fn() -> Result<Terminal<B>> + Send + Sync + 'static,
    {
        Self {
            terminal_factory: Box::new(factory),
            raw_mode_ops: RawModeOps::crossterm(),
            runtime_guard: None,
        }
    }

    #[cfg(test)]
    fn with_factory_and_raw_mode_ops<F>(factory: F, raw_mode_ops: RawModeOps) -> Self
    where
        F: Fn() -> Result<Terminal<B>> + Send + Sync + 'static,
    {
        Self {
            terminal_factory: Box::new(factory),
            raw_mode_ops,
            runtime_guard: None,
        }
    }

    pub fn create_terminal(&self) -> Result<Terminal<B>> {
        (self.terminal_factory)()
    }

    pub fn setup_terminal(&mut self) -> Result<()> {
        if self.runtime_guard.is_none() {
            self.runtime_guard = Some(TerminalRuntimeGuard::install(&self.raw_mode_ops)?);
        }
        Ok(())
    }

    pub fn cleanup_terminal(&mut self) -> Result<()> {
        if let Some(mut guard) = self.runtime_guard.take() {
            guard.cleanup()?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::{TerminalOptions, Viewport};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn cleanup_is_noop_without_setup() {
        let mut core = TerminalCore::with_factory(|| {
            let backend = TestBackend::new(80, 20);
            Terminal::with_options(
                backend,
                TerminalOptions {
                    viewport: Viewport::Inline(20),
                },
            )
            .map_err(Into::into)
        });

        core.cleanup_terminal().unwrap();
        core.cleanup_terminal().unwrap();
    }

    #[test]
    fn setup_and_cleanup_are_idempotent() {
        let enable_calls = Arc::new(AtomicUsize::new(0));
        let disable_calls = Arc::new(AtomicUsize::new(0));

        let raw_mode_ops = RawModeOps {
            enable: {
                let enable_calls = Arc::clone(&enable_calls);
                Arc::new(move || {
                    enable_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            },
            disable: {
                let disable_calls = Arc::clone(&disable_calls);
                Arc::new(move || {
                    disable_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            },
        };

        let mut core = TerminalCore::with_factory_and_raw_mode_ops(
            || {
                let backend = TestBackend::new(80, 20);
                Terminal::with_options(
                    backend,
                    TerminalOptions {
                        viewport: Viewport::Inline(20),
                    },
                )
                .map_err(Into::into)
            },
            raw_mode_ops,
        );

        core.setup_terminal().unwrap();
        core.setup_terminal().unwrap();
        core.cleanup_terminal().unwrap();
        core.cleanup_terminal().unwrap();

        assert_eq!(enable_calls.load(Ordering::SeqCst), 1);
        assert_eq!(disable_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn setup_failure_does_not_install_runtime_guard() {
        let enable_calls = Arc::new(AtomicUsize::new(0));
        let disable_calls = Arc::new(AtomicUsize::new(0));

        let raw_mode_ops = RawModeOps {
            enable: {
                let enable_calls = Arc::clone(&enable_calls);
                Arc::new(move || {
                    enable_calls.fetch_add(1, Ordering::SeqCst);
                    anyhow::bail!("forced setup failure")
                })
            },
            disable: {
                let disable_calls = Arc::clone(&disable_calls);
                Arc::new(move || {
                    disable_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            },
        };

        let mut core = TerminalCore::with_factory_and_raw_mode_ops(
            || {
                let backend = TestBackend::new(80, 20);
                Terminal::with_options(
                    backend,
                    TerminalOptions {
                        viewport: Viewport::Inline(20),
                    },
                )
                .map_err(Into::into)
            },
            raw_mode_ops,
        );

        assert!(core.setup_terminal().is_err());
        core.cleanup_terminal().unwrap();

        assert_eq!(enable_calls.load(Ordering::SeqCst), 1);
        assert_eq!(disable_calls.load(Ordering::SeqCst), 0);
    }
}
