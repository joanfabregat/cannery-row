//! Shared cancellation event with Python asyncio.Event wait semantics.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Notify;

#[derive(Default)]
struct State {
    set: AtomicBool,
    notify: Notify,
}

/// Clones share state. Setting wakes all registered waiters; clearing affects
/// future waits but does not withdraw a wakeup already delivered by a set.
/// An event signals cooperative cancellation; it does not settle or abort tasks.
#[derive(Clone, Default)]
pub struct CancellationEvent(Arc<State>);

impl CancellationEvent {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_set(&self) -> bool {
        self.0.set.load(Ordering::Acquire)
    }

    pub fn set(&self) {
        self.0.set.store(true, Ordering::Release);
        self.0.notify.notify_waiters();
    }

    pub fn clear(&self) {
        self.0.set.store(false, Ordering::Release);
    }

    /// Wait until set, including a transient set followed by clear while this
    /// wait is already registered. There is no polling interval or timeout.
    pub async fn wait(&self) {
        let notified = self.0.notify.notified();
        tokio::pin!(notified);
        // Register before inspecting the flag, so set cannot fall between a
        // false check and first notification polling. notify_waiters preserves
        // already delivered wakeups when clear runs before this task resumes.
        notified.as_mut().enable();
        if !self.is_set() {
            notified.await;
        }
    }
}
