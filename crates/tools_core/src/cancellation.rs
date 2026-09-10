//! One-shot run cancellation, independent of frontend events and error text.

use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use tokio::sync::watch;

#[derive(Debug, thiserror::Error)]
#[error("run cancelled")]
pub struct Cancelled;

/// Cancellation of one run. Clones address the same run; a subsequent run
/// must use a fresh token. A child token (see [`RunCancellation::child`])
/// is cancelled together with its parent but can also be cancelled alone.
#[derive(Clone, Debug)]
pub struct RunCancellation {
    own: Arc<watch::Sender<bool>>,
    /// The tokens of the enclosing runs.
    ancestors: Vec<Arc<watch::Sender<bool>>>,
}

impl Default for RunCancellation {
    fn default() -> Self {
        Self {
            own: Arc::new(watch::channel(false).0),
            ancestors: Vec::new(),
        }
    }
}

impl RunCancellation {
    /// A token for work nested inside this run: cancelling the parent
    /// cancels the child, cancelling the child leaves the parent running.
    pub fn child(&self) -> Self {
        let mut ancestors = self.ancestors.clone();
        ancestors.push(self.own.clone());
        Self {
            own: Arc::new(watch::channel(false).0),
            ancestors,
        }
    }

    pub fn cancel(&self) {
        self.own.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        self.senders().any(|sender| *sender.borrow())
    }

    /// Whether both tokens belong to the same run (not merely to related
    /// runs).
    pub fn same_run(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.own, &other.own)
    }

    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Cancelled.into())
        } else {
            Ok(())
        }
    }

    /// Resolves once this run or any enclosing run is cancelled.
    pub async fn cancelled(&self) {
        let mut waits: Vec<Pin<Box<dyn Future<Output = ()> + Send + '_>>> = self
            .senders()
            .map(|sender| {
                Box::pin(async move {
                    let mut receiver = sender.subscribe();
                    let _ = receiver.wait_for(|cancelled| *cancelled).await;
                }) as Pin<Box<dyn Future<Output = ()> + Send + '_>>
            })
            .collect();
        std::future::poll_fn(|cx| {
            if waits
                .iter_mut()
                .any(|wait| wait.as_mut().poll(cx).is_ready())
            {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await
    }

    fn senders(&self) -> impl Iterator<Item = &Arc<watch::Sender<bool>>> + '_ {
        std::iter::once(&self.own).chain(self.ancestors.iter())
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
        assert!(first.check().is_err());
        assert!(!second.is_cancelled());
        assert!(!first.same_run(&second));
    }

    #[tokio::test]
    async fn a_child_is_cancelled_with_its_parent_but_not_the_other_way_round() {
        let parent = RunCancellation::default();
        let child = parent.child();
        let grandchild = child.child();
        assert!(!child.same_run(&parent));

        child.cancel();
        assert!(child.is_cancelled());
        assert!(grandchild.is_cancelled());
        assert!(!parent.is_cancelled());

        let sibling = parent.child();
        let waiting = tokio::spawn({
            let sibling = sibling.clone();
            async move { sibling.cancelled().await }
        });
        parent.cancel();
        waiting.await.unwrap();
        assert!(sibling.is_cancelled());
    }
}
