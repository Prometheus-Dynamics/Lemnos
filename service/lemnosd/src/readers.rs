//! Runs subscriptions: decides what is read when, reads it, delivers it.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lemnos_core::{MethodId, Reading, Status};
use lemnos_host::Host;
use lemnos_host::schedule::{Action, DevicePace, Refusal, Scheduler};
use lemnos_wire::Frame;

use crate::clock::MonotonicClock;
use crate::conn::Conn;

/// The fastest a colour array (a frame of LEDs, such as a light's `current`) is read: a frame
/// changes at most this often, so the daemon never reads it faster. 20 ms is 50 values a second.
const FRAME_PERIOD_US: u64 = 20_000;

/// How a subscribed method's values come: no reading is read more often than the board's read
/// floor (`floor_us`, [`lemnos_host::Options::read_floor`]), whatever a subscriber asks for, and a
/// colour array no more often than [`FRAME_PERIOD_US`]. Without the floor a subscriber asking for
/// the fastest period has the reading read again the moment its last read ended: a thread of the
/// service, the device's lane and its bus, all spinning for values that seldom change.
pub(crate) fn pace_for(reading: Option<Reading>, floor_us: u64) -> DevicePace {
    let sample_period_us = match reading {
        Some(Reading::ColourArray) => FRAME_PERIOD_US.max(floor_us),
        _ => floor_us,
    };
    DevicePace {
        sample_period_us,
        buffered_samples: 0,
    }
}

/// One method of one device, read on behalf of however many subscribers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Point {
    pub(crate) device: u32,
    pub(crate) method: MethodId,
    /// The method fills an out buffer (a reading such as a colour array): each read passes the
    /// buffer's capacity as its one argument, as a call does.
    pub(crate) out: bool,
}

struct State {
    scheduler: Scheduler,
    points: Vec<Point>,
    owners: HashMap<u64, Arc<Conn>>,
    stop: bool,
}

pub(crate) struct Readers {
    state: Mutex<State>,
    wake: Condvar,
    /// The board's read floor, in microseconds.
    floor_us: u64,
}

const fn refusal_status(refusal: Refusal) -> Status {
    match refusal {
        Refusal::NoDevice => Status::NOT_FOUND,
        Refusal::NoPeriod | Refusal::NoValues => Status::INVALID_ARGUMENT,
    }
}

impl Readers {
    pub(crate) fn new(floor: Duration) -> Self {
        Self {
            floor_us: u64::try_from(floor.as_micros()).unwrap_or(u64::MAX),
            state: Mutex::new(State {
                scheduler: Scheduler::new(),
                points: Vec::new(),
                owners: HashMap::new(),
                stop: false,
            }),
            wake: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds a subscription. `announce` is called with its id and granted period while the
    /// readers are still locked, so the reply that names the subscription is sent before any
    /// value for it can be: a client drops a value for a subscription it has not been told of,
    /// and with `only_changes` the value it lost would not come again.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn subscribe(
        &self,
        owner: &Arc<Conn>,
        wanted: Point,
        period_us: u64,
        only_changes: bool,
        now_us: u64,
        pace: DevicePace,
        announce: impl FnOnce(u64, u64),
    ) -> Result<(), Status> {
        let mut state = self.lock();
        let point = match state.points.iter().position(|p| *p == wanted) {
            Some(point) => point as u32,
            None => {
                let point = state.points.len() as u32;
                state.points.push(wanted);
                state.scheduler.add_device(point, pace);
                point
            }
        };
        let grant = state
            .scheduler
            .subscribe(point, 1, period_us, only_changes, now_us)
            .map_err(refusal_status)?;
        state.owners.insert(grant.id, Arc::clone(owner));
        announce(grant.id, grant.period_us);
        self.wake.notify_all();
        Ok(())
    }

    /// Whether `owner` held that subscription.
    pub(crate) fn unsubscribe(&self, owner: &Arc<Conn>, id: u64, now_us: u64) -> bool {
        let mut state = self.lock();
        if !state.owners.get(&id).is_some_and(|o| Arc::ptr_eq(o, owner)) {
            return false;
        }
        state.owners.remove(&id);
        state.scheduler.unsubscribe(id, now_us);
        true
    }

    /// The pace of a subscribed method with that reading, on this board.
    pub(crate) fn pace_for(&self, reading: Option<Reading>) -> DevicePace {
        pace_for(reading, self.floor_us)
    }

    pub(crate) fn stop(&self) {
        self.lock().stop = true;
        self.wake.notify_all();
    }

    pub(crate) fn run(&self, host: &Host, clock: &MonotonicClock) {
        let mut scratch = vec![0u8; 64 * 1024];
        // An out buffer's capacity, the argument a read of an out-buffer method passes. The scratch
        // is far below a frame's limit, so whatever fills it fits in a value.
        let capacity = u32::try_from(scratch.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes();
        let mut recipients = Vec::new();
        let mut state = self.lock();
        loop {
            if state.stop {
                return;
            }
            let now = clock.now_us();
            match state.scheduler.next(now) {
                Action::Idle => {
                    state = self
                        .wake
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                Action::WaitUntil(at) => {
                    let wait = Duration::from_micros(at.saturating_sub(now));
                    state = self
                        .wake
                        .wait_timeout(state, wait)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                Action::Read(plan) => {
                    let point = state.points[plan.device as usize];
                    drop(state);
                    let started = clock.now_us();
                    let args: &[u8] = if point.out { &capacity } else { &[] };
                    let result = host.call(point.device, point.method, args, &mut scratch);
                    let ended = clock.now_us();
                    state = self.lock();
                    state.scheduler.finished(plan.device, started, ended);
                    let Ok(len) = result else {
                        continue;
                    };
                    recipients.clear();
                    recipients.extend(state.scheduler.subscribers(plan.device).map(|(id, _)| id));
                    let bytes = &scratch[..len];
                    let stamp_ns = ended * 1000;
                    for &subscription in &recipients {
                        if state.scheduler.should_deliver(subscription, bytes)
                            && let Some(owner) = state.owners.get(&subscription)
                        {
                            owner.value(&Frame::Value {
                                subscription,
                                stamp_ns,
                                bytes,
                            });
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOOR_US: u64 = 10_000;

    #[test]
    fn no_reading_is_granted_faster_than_the_floor_and_a_colour_array_no_faster_than_a_frame() {
        assert_eq!(
            pace_for(Some(Reading::ColourArray), FLOOR_US).sample_period_us,
            FRAME_PERIOD_US
        );
        assert_eq!(
            pace_for(Some(Reading::ColourArray), 50_000).sample_period_us,
            50_000,
            "a floor above the frame period holds the colour array too"
        );
        assert_eq!(
            pace_for(Some(Reading::Number), FLOOR_US).sample_period_us,
            FLOOR_US
        );
        assert_eq!(pace_for(None, FLOOR_US).sample_period_us, FLOOR_US);

        let mut capped = Scheduler::new();
        capped.add_device(0, pace_for(Some(Reading::ColourArray), FLOOR_US));
        let grant = capped.subscribe(0, 1, 1_000, true, 0).unwrap();
        assert_eq!(
            grant.period_us, 20_000,
            "a request for 1 ms is granted 20 ms"
        );

        // The fastest a client can ask for, 1 us, is granted the floor; a slower ask is kept.
        let mut floored = Scheduler::new();
        floored.add_device(0, pace_for(Some(Reading::Number), FLOOR_US));
        assert_eq!(
            floored.subscribe(0, 1, 1, true, 0).unwrap().period_us,
            FLOOR_US
        );
        assert_eq!(
            floored.subscribe(0, 1, 250_000, true, 0).unwrap().period_us,
            250_000
        );
    }
}
