//! A channel bounded by the heap its messages hold, not by their number.
//!
//! The streaming scan ([`scan_streaming`](crate::scan::scan_streaming))
//! sends a folder's entries per message. Bounded by the number of messages,
//! the walk could run far ahead of a slow receiver whenever folders were
//! big: in a rescan of a 3.2M-entry disk, where the writer upserts more
//! slowly than eight walkers list, 256 messages held about 45 MB. Here a
//! sender waits while the bytes queued plus its message's would pass the
//! budget, so what the walk holds ahead of the receiver stays near the
//! budget whatever the folders hold.
//!
//! * A message is always admitted into an empty queue, however big, so one
//!   larger than the budget can't stop the walk. The scan sends at most
//!   [`STREAM_CHUNK`](crate::scan::STREAM_CHUNK) entries per message, so
//!   that never goes far past the budget.
//! * Dropping the [`Receiver`] wakes every waiting sender, and every send
//!   from then on hands its message back, so a receiver that stops early (a
//!   sink that panicked) never leaves the walk waiting.
//! * Dropping the [`Sender`] ends the receiver's iteration once the queue
//!   is drained. There is one sender; the walk's threads share it by
//!   reference.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

/// A channel that holds at most about `budget` bytes of queued messages.
/// See the module docs.
#[must_use]
pub fn channel<T>(budget: usize) -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            queue: VecDeque::new(),
            bytes: 0,
            sender_gone: false,
            receiver_gone: false,
        }),
        space: Condvar::new(),
        ready: Condvar::new(),
        budget,
    });
    (
        Sender {
            shared: Arc::clone(&shared),
        },
        Receiver { shared },
    )
}

struct Shared<T> {
    state: Mutex<State<T>>,
    /// Senders wait here for room.
    space: Condvar,
    /// The receiver waits here for a message.
    ready: Condvar,
    budget: usize,
}

struct State<T> {
    /// Each message with the bytes its sender said it holds.
    queue: VecDeque<(T, usize)>,
    /// The sum of the queued messages' bytes.
    bytes: usize,
    sender_gone: bool,
    receiver_gone: bool,
}

impl<T> Shared<T> {
    /// The state, also after a panic on another thread: every update below
    /// leaves it consistent before anything that could panic.
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The sending half of [`channel`]; `Sync`, so the walk's threads share it.
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Sender<T> {
    /// Queues `msg`, which holds `bytes` of heap, first waiting while the
    /// queue is not empty and `msg` would take it past the budget. Hands
    /// `msg` back once the receiver is gone.
    pub fn send(&self, msg: T, bytes: usize) -> Result<(), T> {
        let mut st = self.shared.lock();
        loop {
            if st.receiver_gone {
                return Err(msg);
            }
            if st.queue.is_empty() || st.bytes.saturating_add(bytes) <= self.shared.budget {
                break;
            }
            st = self
                .shared
                .space
                .wait(st)
                .unwrap_or_else(PoisonError::into_inner);
        }
        st.bytes += bytes;
        st.queue.push_back((msg, bytes));
        drop(st);
        self.shared.ready.notify_one();
        Ok(())
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.shared.lock().sender_gone = true;
        self.shared.ready.notify_all();
    }
}

/// The receiving half of [`channel`]. Iterating it yields the messages in
/// the order they were sent and ends once the sender is gone and the queue
/// is empty.
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Receiver<T> {
    /// The next message, waiting for one; `None` once the sender is gone
    /// and every message has been taken.
    pub fn recv(&self) -> Option<T> {
        let mut st = self.shared.lock();
        loop {
            if let Some((msg, bytes)) = st.queue.pop_front() {
                st.bytes -= bytes;
                drop(st);
                // Senders of different sizes may fit now: wake them all.
                self.shared.space.notify_all();
                return Some(msg);
            }
            if st.sender_gone {
                return None;
            }
            st = self
                .shared
                .ready
                .wait(st)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl<T> Iterator for Receiver<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.recv()
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let queued = {
            let mut st = self.shared.lock();
            st.receiver_gone = true;
            st.bytes = 0;
            std::mem::take(&mut st.queue)
        };
        self.shared.space.notify_all();
        // The messages left go outside the lock.
        drop(queued);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn messages_arrive_in_order_and_end_with_the_sender() {
        let (tx, rx) = channel(100);
        let walk = thread::spawn(move || {
            for i in 0..1000 {
                tx.send(i, 10).unwrap();
            }
        });
        let got: Vec<i32> = rx.collect();
        walk.join().unwrap();
        assert_eq!(got, (0..1000).collect::<Vec<_>>());
    }

    #[test]
    fn a_sender_waits_while_the_budget_is_full() {
        let (tx, rx) = channel(100);
        let sent = AtomicUsize::new(0);
        thread::scope(|s| {
            s.spawn(|| {
                for i in 0..10 {
                    tx.send(i, 40).unwrap();
                    sent.fetch_add(1, Ordering::SeqCst);
                }
            });
            // Two messages of 40 B fit in 100 B; a third would not.
            thread::sleep(Duration::from_millis(200));
            assert_eq!(sent.load(Ordering::SeqCst), 2);
            assert_eq!(rx.recv(), Some(0));
            thread::sleep(Duration::from_millis(200));
            assert_eq!(sent.load(Ordering::SeqCst), 3);
            let rest: Vec<i32> = (0..9).map(|_| rx.recv().unwrap()).collect();
            assert_eq!(rest, (1..10).collect::<Vec<_>>());
        });
        drop(tx);
        assert_eq!(rx.recv(), None);
    }

    #[test]
    fn a_message_bigger_than_the_budget_goes_into_an_empty_queue() {
        let (tx, rx) = channel(100);
        tx.send("big", 1_000).unwrap();
        assert_eq!(rx.recv(), Some("big"));
        tx.send("bigger", 5_000).unwrap();
        drop(tx);
        assert_eq!(rx.collect::<Vec<_>>(), vec!["bigger"]);
    }

    #[test]
    fn dropping_the_receiver_releases_a_waiting_sender() {
        let (tx, rx) = channel(10);
        thread::scope(|s| {
            let walk = s.spawn(|| {
                tx.send(1, 10).unwrap();
                // The queue is full: this one waits until the receiver goes.
                tx.send(2, 10)
            });
            thread::sleep(Duration::from_millis(100));
            drop(rx);
            assert_eq!(walk.join().unwrap(), Err(2));
        });
        assert_eq!(tx.send(3, 0), Err(3));
    }
}
