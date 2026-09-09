//! One-shot run cancellation, independent of frontend events and error text.
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Debug, thiserror::Error)]
#[error("run cancelled")]
pub struct Cancelled;

/// Clones address the same run. A subsequent run must use a fresh token.
#[derive(Clone, Debug)]
pub struct RunCancellation(Arc<watch::Sender<bool>>);

impl Default for RunCancellation {
    fn default() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }
}

impl RunCancellation {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    pub fn same_run(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Cancelled.into())
        } else {
            Ok(())
        }
    }

    pub async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }

    /// Linearize a short synchronous publication with cancellation. Never
    /// await or cancel this token from inside `f`.
    pub fn if_active<T>(&self, f: impl FnOnce() -> T) -> Result<T> {
        let cancelled = self.0.borrow();
        if *cancelled {
            return Err(Cancelled.into());
        }
        Ok(f())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_is_sticky_and_run_local() {
        let first = RunCancellation::default();
        let second = RunCancellation::default();
        first.cancel();
        first.cancelled().await;
        assert!(
            first
                .if_active(|| panic!("publication after stop"))
                .is_err()
        );
        assert!(!second.is_cancelled());
        assert!(!first.same_run(&second));
    }
}
