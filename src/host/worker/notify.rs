//! [`WorkerNotifier`]: a worker's agents notify nobody on the box itself.
//!
//! On a TUI or Desktop host a finished agent posts an OS notification. A
//! worker runs headless, so the same notifications (the same
//! `take_finish_notifications` edges, honouring `[notifications]`) are queued
//! here instead, and the host forwards them to the Desktops that are
//! connected, which decide where to show them.

use std::sync::{Arc, Mutex};

use crate::contracts::{Notification, Notifier};

/// A [`Notifier`] that queues instead of posting.
#[derive(Debug, Default, Clone)]
pub struct WorkerNotifier {
    queue: Arc<Mutex<Vec<Notification>>>,
}

impl WorkerNotifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// The shared queue, for the host to drain each turn.
    pub fn queue(&self) -> Arc<Mutex<Vec<Notification>>> {
        Arc::clone(&self.queue)
    }

    /// Everything queued since the last call, oldest first.
    pub fn take(&self) -> Vec<Notification> {
        match self.queue.lock() {
            Ok(mut q) => std::mem::take(&mut *q),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }
}

impl Notifier for WorkerNotifier {
    fn notify(&self, notification: &Notification) {
        match self.queue.lock() {
            Ok(mut q) => q.push(notification.clone()),
            Err(poisoned) => poisoned.into_inner().push(notification.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::NotificationSound;

    fn note(title: &str) -> Notification {
        Notification {
            title: title.to_string(),
            body: "Claude Code finished".to_string(),
            sound: NotificationSound::Completion,
        }
    }

    #[test]
    fn notifications_queue_in_order_and_drain_once() {
        let n = WorkerNotifier::new();
        n.notify(&note("a"));
        n.notify(&note("b"));
        assert_eq!(n.take(), vec![note("a"), note("b")]);
        assert!(n.take().is_empty());
    }

    #[test]
    fn the_shared_queue_sees_the_same_notifications() {
        let n = WorkerNotifier::new();
        let queue = n.queue();
        n.notify(&note("a"));
        assert_eq!(queue.lock().unwrap().len(), 1);
        assert_eq!(n.take(), vec![note("a")]);
        assert!(queue.lock().unwrap().is_empty());
    }
}
