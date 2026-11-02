//! Blocking on a poll function from a plain thread: the crate has no async
//! runtime of its own. Transfers and hotplug events are futures, so the same
//! wakeup serves `.await` and the blocking calls.

use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

/// A waker that unparks the thread that created it.
pub(crate) struct ThreadWaker(Thread);

impl ThreadWaker {
    /// A waker for the current thread.
    pub(crate) fn current() -> Waker {
        Waker::from(Arc::new(Self(thread::current())))
    }
}

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Polls `poll` on this thread until it is ready.
pub(crate) fn block_on_poll<T>(mut poll: impl FnMut(&mut Context<'_>) -> Poll<T>) -> T {
    let waker = ThreadWaker::current();
    let mut context = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(value) = poll(&mut context) {
            return value;
        }
        thread::park();
    }
}
