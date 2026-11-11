//! The control node's event stream, shared by every interrupt a process holds.
//!
//! The module queues one event per interrupt that fired, for every block, on the one node. So
//! whoever reads it reads everyone's: each event is filed under the interrupt it is for, and the
//! holder of that interrupt takes it from there. One waiter at a time polls the node; the others
//! wait to be told something was filed. Filing and taking allocate nothing: the table has a slot
//! per interrupt held, made when it was opened.
//!
//! Events of other kinds (a supply fault, a temperature crossed, a block bound) have no reader in
//! this backend and are dropped. A block unbound marks its interrupts gone.

use std::io;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::InterruptEvent;
use lemnos_linux_sys::module::{self as sys, EVENT_BLOCK_UNBOUND, EVENT_IRQ, Event};
use lemnos_linux_sys::poll;

use crate::kind_of;

/// Events read per `read`, the module's own batch.
const BATCH: usize = 8;

/// The interrupts held, and whether someone is polling the node for them.
#[derive(Default)]
pub(crate) struct Events {
    table: Mutex<Table>,
    filed: Condvar,
}

#[derive(Default)]
struct Table {
    slots: Vec<Slot>,
    polling: bool,
}

/// One interrupt held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Slot {
    block: u32,
    index: u32,
    /// The module's count at the last event, so the next says how many fired in between.
    last: Option<u32>,
    /// Fired and not yet taken.
    fired: u32,
    ns: u64,
    /// Events are kept only while the holder wants them.
    enabled: bool,
    /// The block was unbound.
    gone: bool,
}

impl Table {
    fn slot(&mut self, block: u32, index: u32) -> Option<&mut Slot> {
        self.slots
            .iter_mut()
            .find(|slot| slot.block == block && slot.index == index)
    }

    /// Files one event under the interrupt it is for.
    fn file(&mut self, event: &Event) {
        match event.kind {
            EVENT_IRQ => {
                if let Some(slot) = self.slot(event.block, event.index) {
                    // The count is the module's since the block was bound: what fired since the
                    // last is the difference, and at least this one.
                    let since = slot
                        .last
                        .map_or(1, |last| event.count.wrapping_sub(last).max(1));
                    slot.last = Some(event.count);
                    if slot.enabled {
                        slot.fired = slot.fired.saturating_add(since);
                        slot.ns = event.ns;
                    }
                }
            }
            EVENT_BLOCK_UNBOUND => {
                for slot in self.slots.iter_mut().filter(|s| s.block == event.block) {
                    slot.gone = true;
                }
            }
            _ => {}
        }
    }

    /// What `block`'s interrupt `index` has waiting: an event, nothing yet, or `Unavailable` once
    /// the block is gone.
    fn take(&mut self, block: u32, index: u32) -> Result<Option<InterruptEvent>, ErrorKind> {
        let slot = self.slot(block, index).ok_or(ErrorKind::NotFound)?;
        if slot.fired > 0 {
            let event = InterruptEvent {
                count: slot.fired,
                timestamp_ns: slot.ns,
            };
            slot.fired = 0;
            return Ok(Some(event));
        }
        if slot.gone {
            return Err(ErrorKind::Unavailable);
        }
        Ok(None)
    }
}

impl Events {
    fn lock(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Makes a slot for `block`'s interrupt `index`, disabled. One holder per interrupt: the
    /// caller holds its claim.
    pub(crate) fn register(&self, block: u32, index: u32) {
        let mut table = self.lock();
        table
            .slots
            .retain(|s| !(s.block == block && s.index == index));
        table.slots.push(Slot {
            block,
            index,
            last: None,
            fired: 0,
            ns: 0,
            enabled: false,
            gone: false,
        });
    }

    pub(crate) fn unregister(&self, block: u32, index: u32) {
        self.lock()
            .slots
            .retain(|s| !(s.block == block && s.index == index));
    }

    /// Whether events for the interrupt are kept. Turning them off drops what was waiting.
    pub(crate) fn set_enabled(&self, block: u32, index: u32, enabled: bool) {
        if let Some(slot) = self.lock().slot(block, index) {
            slot.enabled = enabled;
            if !enabled {
                slot.fired = 0;
            }
        }
    }

    /// Reads everything waiting on the node and files it.
    fn drain(table: &mut Table, node: std::os::fd::BorrowedFd<'_>) -> Result<(), ErrorKind> {
        let mut batch = [Event::default(); BATCH];
        loop {
            match sys::read_events(node, &mut batch) {
                Ok(0) => return Ok(()),
                Ok(n) => batch[..n].iter().for_each(|event| table.file(event)),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(kind_of(&e)),
            }
        }
    }

    /// Waits for `block`'s interrupt `index`, up to `timeout` (`None`: without limit; zero: only
    /// checks). Allocates nothing.
    pub(crate) fn wait(
        &self,
        node: std::os::fd::BorrowedFd<'_>,
        (block, index): (u32, u32),
        timeout: Option<Duration>,
    ) -> Result<Option<InterruptEvent>, ErrorKind> {
        let deadline = timeout.map(|t| Instant::now().checked_add(t));
        let left = || match deadline {
            None => Some(None),
            // Too far away to say: as good as no limit.
            Some(None) => Some(None),
            Some(Some(at)) => {
                let left = at.saturating_duration_since(Instant::now());
                (!left.is_zero()).then_some(Some(left))
            }
        };
        let mut table = self.lock();
        loop {
            Self::drain(&mut table, node)?;
            if let Some(event) = table.take(block, index)? {
                return Ok(Some(event));
            }
            let Some(left) = left() else {
                return Ok(None);
            };
            if table.polling {
                // Someone else is reading the node: they file what comes and say so.
                table = match left {
                    None => self
                        .filed
                        .wait(table)
                        .unwrap_or_else(PoisonError::into_inner),
                    Some(left) => {
                        self.filed
                            .wait_timeout(table, left)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0
                    }
                };
                continue;
            }
            table.polling = true;
            drop(table);
            let polled = poll::poll_one(node, poll::POLLIN, left);
            table = self.lock();
            table.polling = false;
            let drained = Self::drain(&mut table, node);
            self.filed.notify_all();
            polled.map_err(|e| kind_of(&e))?;
            drained?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn irq(block: u32, index: u32, count: u32, ns: u64) -> Event {
        Event {
            block,
            index,
            count,
            kind: EVENT_IRQ,
            ns,
            value: 0,
        }
    }

    fn table(enabled: bool) -> Table {
        let events = Events::default();
        events.register(3, 0);
        events.register(3, 1);
        events.register(4, 0);
        events.set_enabled(3, 0, enabled);
        events.set_enabled(3, 1, true);
        events.set_enabled(4, 0, true);
        events.table.into_inner().unwrap()
    }

    #[test]
    fn each_event_goes_to_its_own_interrupt() {
        let mut table = table(true);
        table.file(&irq(3, 1, 1, 10));
        table.file(&irq(4, 0, 1, 20));
        table.file(&irq(9, 0, 1, 30));
        assert_eq!(table.take(3, 0), Ok(None));
        assert_eq!(
            table.take(3, 1),
            Ok(Some(InterruptEvent {
                count: 1,
                timestamp_ns: 10
            }))
        );
        assert_eq!(table.take(3, 1), Ok(None));
        assert_eq!(
            table.take(4, 0),
            Ok(Some(InterruptEvent {
                count: 1,
                timestamp_ns: 20
            }))
        );
        assert_eq!(table.take(9, 0), Err(ErrorKind::NotFound));
    }

    #[test]
    fn the_count_says_how_many_fired_since_the_last_taken() {
        let mut table = table(true);
        // The first event seen counts as one, whatever the module had counted before.
        table.file(&irq(3, 0, 41, 1));
        table.file(&irq(3, 0, 44, 2));
        assert_eq!(
            table.take(3, 0),
            Ok(Some(InterruptEvent {
                count: 4,
                timestamp_ns: 2
            }))
        );
        // The module's count wraps.
        let mut table = super::tests::table(true);
        table.file(&irq(3, 0, u32::MAX, 3));
        assert_eq!(table.take(3, 0).unwrap().unwrap().count, 1);
        table.file(&irq(3, 0, 1, 4));
        assert_eq!(table.take(3, 0).unwrap().unwrap().count, 2);
        // A repeat of the same count is still one that arrived.
        table.file(&irq(3, 0, 1, 5));
        assert_eq!(table.take(3, 0).unwrap().unwrap().count, 1);
    }

    #[test]
    fn a_disabled_interrupt_keeps_nothing() {
        let mut table = table(false);
        table.file(&irq(3, 0, 1, 1));
        assert_eq!(table.take(3, 0), Ok(None));
    }

    #[test]
    fn an_unbound_block_is_gone_once_its_events_are_taken() {
        let mut table = table(true);
        table.file(&irq(3, 0, 1, 1));
        table.file(&Event {
            block: 3,
            kind: EVENT_BLOCK_UNBOUND,
            ..Event::default()
        });
        assert!(table.take(3, 0).unwrap().is_some());
        assert_eq!(table.take(3, 0), Err(ErrorKind::Unavailable));
        assert_eq!(table.take(3, 1), Err(ErrorKind::Unavailable));
        assert_eq!(table.take(4, 0), Ok(None));
    }

    #[test]
    fn filing_and_taking_allocate_nothing() {
        let mut table = table(true);
        let ((), count) = lemnos_measure::test_alloc::allocations(|| {
            for n in 0..1000 {
                table.file(&irq(3, 0, n, u64::from(n)));
                table.file(&irq(4, 0, n, u64::from(n)));
                let _ = table.take(3, 0);
            }
        });
        assert_eq!(count, 0);
    }

    #[test]
    fn waiting_on_a_node_with_nothing_times_out() {
        // A non-blocking socket with nothing sent reads and polls as the node does with no
        // events waiting.
        let (read, _write) = std::os::unix::net::UnixStream::pair().unwrap();
        read.set_nonblocking(true).unwrap();
        let fd = std::os::fd::AsFd::as_fd(&read);
        let events = Events::default();
        events.register(1, 0);
        events.set_enabled(1, 0, true);
        let start = Instant::now();
        assert_eq!(
            events.wait(fd, (1, 0), Some(Duration::from_millis(20))),
            Ok(None)
        );
        assert!(start.elapsed() >= Duration::from_millis(20));
        assert_eq!(events.wait(fd, (1, 0), Some(Duration::ZERO)), Ok(None));
    }
}
