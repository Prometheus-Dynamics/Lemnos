use crate::runtime::Runtime;
use lemnos_core::LemnosEvent;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RuntimeEventCursor {
    next_index: usize,
}

impl RuntimeEventCursor {
    pub fn from_start() -> Self {
        Self::default()
    }

    pub(crate) fn from_index(next_index: usize) -> Self {
        Self { next_index }
    }

    pub fn next_index(&self) -> usize {
        self.next_index
    }

    pub(crate) fn advance_to(&mut self, next_index: usize) {
        self.next_index = next_index;
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeEventSubscription {
    cursor: RuntimeEventCursor,
    observed_version: usize,
    notifier: Arc<RuntimeEventNotifier>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeEventPoll<'a> {
    events: &'a [LemnosEvent],
    was_truncated: bool,
}

impl<'a> RuntimeEventPoll<'a> {
    pub(crate) fn new(events: &'a [LemnosEvent], was_truncated: bool) -> Self {
        Self {
            events,
            was_truncated,
        }
    }

    pub fn events(&self) -> &'a [LemnosEvent] {
        self.events
    }

    pub fn was_truncated(&self) -> bool {
        self.was_truncated
    }
}

impl RuntimeEventSubscription {
    pub(crate) fn new(cursor: RuntimeEventCursor, notifier: Arc<RuntimeEventNotifier>) -> Self {
        let observed_version = if cursor.next_index() < notifier.current_tail_index() {
            notifier.current_version().saturating_sub(1)
        } else {
            notifier.current_version()
        };
        Self {
            cursor,
            observed_version,
            notifier,
        }
    }

    pub fn cursor(&self) -> RuntimeEventCursor {
        self.cursor
    }

    pub fn next_index(&self) -> usize {
        self.cursor.next_index()
    }

    pub fn is_stale(&self, runtime: &Runtime) -> bool {
        runtime.is_cursor_stale(&self.cursor)
    }

    pub fn has_pending(&self, runtime: &Runtime) -> bool {
        self.pending_count(runtime) > 0
    }

    pub fn pending_count(&self, runtime: &Runtime) -> usize {
        runtime.pending_event_count(&self.cursor)
    }

    pub fn poll<'a>(&mut self, runtime: &'a Runtime) -> &'a [LemnosEvent] {
        self.poll_with_status(runtime).events()
    }

    pub fn poll_with_status<'a>(&mut self, runtime: &'a Runtime) -> RuntimeEventPoll<'a> {
        let poll = runtime.poll_events_with_status(&mut self.cursor);
        self.observed_version = self.notifier.current_version();
        poll
    }

    pub fn wait_for_update(&self, timeout: Option<Duration>) -> bool {
        self.notifier
            .wait_for_change_after(self.observed_version, timeout)
    }

    pub fn wait_and_poll_next<'a>(
        &mut self,
        runtime: &'a Runtime,
        timeout: Option<Duration>,
    ) -> Option<&'a [LemnosEvent]> {
        self.wait_and_poll_with_status(runtime, timeout)
            .map(|poll| poll.events())
    }

    pub fn wait_and_poll_next_with_status<'a>(
        &mut self,
        runtime: &'a Runtime,
        timeout: Option<Duration>,
    ) -> Option<RuntimeEventPoll<'a>> {
        self.wait_and_poll_with_status(runtime, timeout)
    }

    pub fn wait_and_poll_with_status<'a>(
        &mut self,
        runtime: &'a Runtime,
        timeout: Option<Duration>,
    ) -> Option<RuntimeEventPoll<'a>> {
        if self.has_pending(runtime) || self.wait_for_update(timeout) {
            Some(self.poll_with_status(runtime))
        } else {
            None
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct RuntimeEventNotifier {
    state: Mutex<RuntimeEventNotifierState>,
    changed: Condvar,
}

#[derive(Debug, Clone, Copy, Default)]
struct RuntimeEventNotifierState {
    tail_index: usize,
    version: usize,
}

impl RuntimeEventNotifier {
    pub(crate) fn current_tail_index(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tail_index
    }

    pub(crate) fn current_version(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .version
    }

    pub(crate) fn advance_to(&self, tail_index: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if tail_index <= state.tail_index {
            return;
        }
        state.tail_index = tail_index;
        state.version = state.version.saturating_add(1);
        self.changed.notify_all();
    }

    pub(crate) fn notify_changed(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.version = state.version.saturating_add(1);
        self.changed.notify_all();
    }

    fn wait_for_change_after(&self, observed_version: usize, timeout: Option<Duration>) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.version > observed_version {
            return true;
        }

        match timeout {
            None => {
                while state.version <= observed_version {
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                true
            }
            Some(timeout) => {
                let deadline = Instant::now() + timeout;
                let mut remaining = timeout;
                while state.version <= observed_version {
                    let (next_state, result) = self
                        .changed
                        .wait_timeout(state, remaining)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state = next_state;
                    if state.version > observed_version {
                        return true;
                    }
                    if result.timed_out() {
                        return false;
                    }

                    remaining = deadline.saturating_duration_since(Instant::now());
                }
                true
            }
        }
    }
}
