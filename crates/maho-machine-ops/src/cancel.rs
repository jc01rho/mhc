//! A small cloneable cancellation token. It stands in for the `AbortSignal` every senpi operation
//! receives, so the crate needs no dependency beyond tokio.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

#[derive(Debug, Default)]
struct Inner {
    cancelled: AtomicBool,
    notify: Notify,
}

/// Cancelled once, observed by every clone. Cancelling twice is harmless.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    inner: Arc<Inner>,
}

impl CancellationToken {
    /// A token that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel the token and wake everything waiting in [`CancellationToken::cancelled`].
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }

    /// Whether [`CancellationToken::cancel`] was called on this token or any clone.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Resolves once the token is cancelled.
    pub async fn cancelled(&self) {
        loop {
            // `Notified` receives `notify_waiters` wake-ups from the moment it is created, so a cancel
            // between this line and the flag check below cannot be missed.
            let notified = self.inner.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_clone_observes_the_cancel_and_waiters_wake() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        let clone = token.clone();
        let waiter = tokio::spawn(async move { clone.cancelled().await });
        token.cancel();
        waiter.await.unwrap();
        assert!(token.is_cancelled());
        // An already-cancelled token resolves at once.
        token.cancelled().await;
    }
}
