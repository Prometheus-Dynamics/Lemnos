//! The streams a session has handed out and not yet been given back.
//!
//! Closing a [`FrameStream`](crate::FrameStream) sends its close. Dropping one without closing it
//! must do the same, but the stream can't write to the socket (the session owns it). So the drop
//! queues the close here, and the session sends the queued closes with its next write. Nothing
//! waits for their answers.
//!
//! A hold is identified by a token, so a stream that is no longer the holder (its name was taken by
//! a later open on the same connection) can never close the later one.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// One stream a session holds: under which connection it was opened, and where it is.
#[derive(Debug)]
struct Held {
    token: u64,
    generation: u64,
    device: u32,
    name: String,
}

/// A close the session has to send: a stream dropped while its hold was current.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Closing {
    pub(crate) generation: u64,
    pub(crate) device: u32,
    pub(crate) name: String,
}

#[derive(Debug, Default)]
struct Inner {
    next: u64,
    held: Vec<Held>,
    closing: Vec<Closing>,
}

/// The holds of one session, shared with the streams it made.
#[derive(Debug, Default)]
pub(crate) struct Holds {
    inner: Mutex<Inner>,
}

fn lock(holds: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    holds.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Holds {
    /// Records a stream opened on `device`'s `name` under connection `generation`. An earlier hold
    /// of the same name is superseded: the service can only have granted this one if it let go of
    /// that. Returns the token the stream keeps.
    pub(crate) fn register(&self, generation: u64, device: u32, name: &str) -> u64 {
        let mut inner = lock(&self.inner);
        inner
            .held
            .retain(|hold| hold.device != device || hold.name != name);
        inner.next += 1;
        let token = inner.next;
        inner.held.push(Held {
            token,
            generation,
            device,
            name: name.to_string(),
        });
        token
    }

    /// Ends a hold because the stream is being closed explicitly. Returns whether it was current.
    pub(crate) fn release(&self, token: u64) -> bool {
        let mut inner = lock(&self.inner);
        let before = inner.held.len();
        inner.held.retain(|hold| hold.token != token);
        inner.held.len() != before
    }

    /// Ends a hold because its stream was dropped without a close: queues the close, if the hold
    /// was still current.
    pub(crate) fn dropped(&self, token: u64) {
        let mut inner = lock(&self.inner);
        if let Some(index) = inner.held.iter().position(|hold| hold.token == token) {
            let held = inner.held.swap_remove(index);
            inner.closing.push(Closing {
                generation: held.generation,
                device: held.device,
                name: held.name,
            });
        }
    }

    /// The closes queued since the last call, for the session to send or discard.
    pub(crate) fn take_closing(&self) -> Vec<Closing> {
        std::mem::take(&mut lock(&self.inner).closing)
    }
}

/// A stream's end of its hold: released by closing the stream, queued when it is dropped.
#[derive(Debug)]
pub(crate) struct Hold {
    holds: Arc<Holds>,
    token: u64,
}

impl Hold {
    pub(crate) fn new(holds: Arc<Holds>, token: u64) -> Self {
        Self { holds, token }
    }

    /// The stream is being closed explicitly; no close is queued for it.
    pub(crate) fn release(&self) {
        self.holds.release(self.token);
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.holds.dropped(self.token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_hold_queues_one_close_for_its_connection() {
        let holds = Arc::new(Holds::default());
        let token = holds.register(1, 0, "frames");
        let hold = Hold::new(Arc::clone(&holds), token);
        drop(hold);
        assert_eq!(
            holds.take_closing(),
            [Closing {
                generation: 1,
                device: 0,
                name: "frames".into()
            }]
        );
        assert!(holds.take_closing().is_empty());
    }

    #[test]
    fn a_released_hold_queues_nothing() {
        let holds = Arc::new(Holds::default());
        let token = holds.register(1, 0, "frames");
        let hold = Hold::new(Arc::clone(&holds), token);
        hold.release();
        drop(hold);
        assert!(holds.take_closing().is_empty());
    }

    #[test]
    fn a_superseded_hold_queues_nothing_when_it_drops() {
        let holds = Arc::new(Holds::default());
        let old = Hold::new(Arc::clone(&holds), holds.register(1, 0, "frames"));
        let _new = holds.register(1, 0, "frames");
        drop(old);
        assert!(holds.take_closing().is_empty());
    }

    #[test]
    fn holds_of_other_streams_are_kept_apart() {
        let holds = Arc::new(Holds::default());
        let a = Hold::new(Arc::clone(&holds), holds.register(1, 0, "frames"));
        let _b = holds.register(1, 0, "other");
        drop(a);
        assert_eq!(holds.take_closing().len(), 1);
    }
}
