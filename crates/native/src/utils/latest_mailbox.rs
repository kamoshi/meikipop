use std::fmt;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

struct MailboxState<T> {
    pending: Option<MailboxItem<T>>,
    version: u64,
    closed: bool,
}

/// A value received from a [`LatestMailbox`].
///
/// `version` identifies the order in which the mailbox accepted values. It is
/// useful for diagnostics, but domain-level freshness should still be carried
/// by the value itself.
#[derive(Debug, PartialEq, Eq)]
pub struct MailboxItem<T> {
    pub version: u64,
    pub value: T,
}

/// Returned when sending to a closed mailbox.
///
/// The rejected value is returned to the caller rather than silently dropped.
#[derive(PartialEq, Eq)]
pub struct SendError<T>(pub T);

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SendError(..)")
    }
}

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("latest mailbox is closed")
    }
}

impl<T: fmt::Debug> std::error::Error for SendError<T> {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TryRecvError {
    Empty,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecvTimeoutError {
    Timeout,
    Closed,
}

/// A synchronous, single-consumer mailbox for replaceable state.
///
/// The mailbox retains at most one pending value. Sending never waits for the
/// consumer: a new value replaces and returns any older pending value. Receiving
/// atomically waits for and removes the newest pending value.
///
/// This is intended for live state such as captured frames or desired OCR work,
/// where an unprocessed older value becomes obsolete when a newer one arrives.
/// It is not suitable for commands or events that must all be delivered.
///
/// Although calls are thread-safe, only one thread should call [`Self::recv_latest`].
pub struct LatestMailbox<T> {
    state: Mutex<MailboxState<T>>,
    changed: Condvar,
}

impl<T> Default for LatestMailbox<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(MailboxState {
                pending: None,
                version: 0,
                closed: false,
            }),
            changed: Condvar::new(),
        }
    }
}

impl<T> LatestMailbox<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores `value` and wakes the consumer without blocking for capacity.
    ///
    /// Returns the obsolete pending value when one was replaced. If the mailbox
    /// is closed, returns `value` in [`SendError`] and leaves the mailbox unchanged.
    pub fn send_replace(&self, value: T) -> Result<Option<T>, SendError<T>> {
        let mut state = self.lock();
        if state.closed {
            return Err(SendError(value));
        }

        state.version = state.version.wrapping_add(1);
        let version = state.version;
        let replaced = state
            .pending
            .replace(MailboxItem { version, value })
            .map(|item| item.value);
        self.changed.notify_one();
        Ok(replaced)
    }

    /// Waits for and removes the newest pending value.
    ///
    /// Returns `None` after [`Self::close`] has been called. Closing discards any
    /// still-pending value so shutdown cannot accidentally start obsolete work.
    pub fn recv_latest(&self) -> Option<MailboxItem<T>> {
        let mut state = self.lock();
        loop {
            if state.closed {
                return None;
            }
            if let Some(item) = state.pending.take() {
                return Some(item);
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    /// Removes the newest pending value without waiting.
    pub fn try_recv_latest(&self) -> Result<MailboxItem<T>, TryRecvError> {
        let mut state = self.lock();
        if state.closed {
            return Err(TryRecvError::Closed);
        }
        state.pending.take().ok_or(TryRecvError::Empty)
    }

    /// Waits up to `timeout` for and removes the newest pending value.
    pub fn recv_latest_timeout(
        &self,
        timeout: Duration,
    ) -> Result<MailboxItem<T>, RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if state.closed {
                return Err(RecvTimeoutError::Closed);
            }
            if let Some(item) = state.pending.take() {
                return Ok(item);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(RecvTimeoutError::Timeout);
            }
            let (next_state, wait_result) = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|error| error.into_inner());
            state = next_state;
            if wait_result.timed_out() && state.pending.is_none() {
                return if state.closed {
                    Err(RecvTimeoutError::Closed)
                } else {
                    Err(RecvTimeoutError::Timeout)
                };
            }
        }
    }

    /// Closes the mailbox, discards and returns pending work, and wakes a receiver.
    ///
    /// Calling this more than once is harmless. Values already removed by the
    /// receiver are owned by that receiver and cannot be cancelled here.
    pub fn close(&self) -> Option<T> {
        let mut state = self.lock();
        state.closed = true;
        let pending = state.pending.take().map(|item| item.value);
        self.changed.notify_all();
        pending
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    fn lock(&self) -> MutexGuard<'_, MailboxState<T>> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[test]
    fn sending_replaces_and_returns_obsolete_pending_work() {
        let mailbox = LatestMailbox::new();

        assert_eq!(mailbox.send_replace("old"), Ok(None));
        assert_eq!(mailbox.send_replace("latest"), Ok(Some("old")));
        assert_eq!(
            mailbox.recv_latest(),
            Some(MailboxItem {
                version: 2,
                value: "latest",
            })
        );
    }

    #[test]
    fn receiver_blocks_until_a_value_arrives() {
        let mailbox = Arc::new(LatestMailbox::new());
        let receiver_mailbox = Arc::clone(&mailbox);
        let (ready_sender, ready_receiver) = mpsc::channel();
        let receiver = thread::spawn(move || {
            ready_sender.send(()).unwrap();
            receiver_mailbox.recv_latest()
        });

        ready_receiver.recv().unwrap();
        assert_eq!(mailbox.send_replace(42), Ok(None));

        assert_eq!(
            receiver.join().unwrap(),
            Some(MailboxItem {
                version: 1,
                value: 42,
            })
        );
    }

    #[test]
    fn close_discards_pending_work_and_rejects_future_sends() {
        let mailbox = LatestMailbox::new();
        mailbox.send_replace("pending").unwrap();

        assert_eq!(mailbox.close(), Some("pending"));
        assert!(mailbox.is_closed());
        assert_eq!(mailbox.recv_latest(), None);
        assert_eq!(mailbox.send_replace("rejected"), Err(SendError("rejected")));
        assert_eq!(mailbox.close(), None);
    }

    #[test]
    fn close_wakes_a_blocked_receiver() {
        let mailbox = Arc::new(LatestMailbox::<()>::new());
        let receiver_mailbox = Arc::clone(&mailbox);
        let (ready_sender, ready_receiver) = mpsc::channel();
        let receiver = thread::spawn(move || {
            ready_sender.send(()).unwrap();
            receiver_mailbox.recv_latest()
        });

        ready_receiver.recv().unwrap();
        mailbox.close();

        let (done_sender, done_receiver) = mpsc::channel();
        thread::spawn(move || done_sender.send(receiver.join().unwrap()).unwrap());
        assert_eq!(
            done_receiver.recv_timeout(Duration::from_secs(1)),
            Ok(None),
            "closing the mailbox should promptly wake its receiver"
        );
    }

    #[test]
    fn try_receive_distinguishes_empty_from_closed() {
        let mailbox = LatestMailbox::new();

        assert_eq!(mailbox.try_recv_latest(), Err(TryRecvError::Empty));
        mailbox.send_replace(7).unwrap();
        assert_eq!(mailbox.try_recv_latest().unwrap().value, 7);
        mailbox.close();
        assert_eq!(mailbox.try_recv_latest(), Err(TryRecvError::Closed));
    }

    #[test]
    fn timed_receive_reports_timeout_and_close() {
        let mailbox = LatestMailbox::<()>::new();

        assert_eq!(
            mailbox.recv_latest_timeout(Duration::from_millis(1)),
            Err(RecvTimeoutError::Timeout)
        );
        mailbox.close();
        assert_eq!(
            mailbox.recv_latest_timeout(Duration::from_secs(1)),
            Err(RecvTimeoutError::Closed)
        );
    }
}
