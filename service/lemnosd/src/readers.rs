//! Runs subscriptions: decides what is read when, reads it, delivers it.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use lemnos_core::{MethodId, Status};
use lemnos_host::Host;
use lemnos_host::schedule::{Action, DevicePace, Refusal, Scheduler};
use lemnos_wire::Frame;

use crate::clock::MonotonicClock;
use crate::conn::Conn;

/// One method of one device, read on behalf of however many subscribers.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Point {
    device: u32,
    method: MethodId,
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
}

const fn refusal_status(refusal: Refusal) -> Status {
    match refusal {
        Refusal::NoDevice => Status::NOT_FOUND,
        Refusal::NoPeriod | Refusal::NoValues => Status::INVALID_ARGUMENT,
    }
}

impl Readers {
    pub(crate) fn new() -> Self {
        Self {
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

    pub(crate) fn subscribe(
        &self,
        owner: &Arc<Conn>,
        device: u32,
        method: MethodId,
        period_us: u64,
        only_changes: bool,
        now_us: u64,
    ) -> Result<(u64, u64), Status> {
        let mut state = self.lock();
        let wanted = Point { device, method };
        let point = match state.points.iter().position(|p| *p == wanted) {
            Some(point) => point as u32,
            None => {
                let point = state.points.len() as u32;
                state.points.push(wanted);
                state.scheduler.add_device(
                    point,
                    DevicePace {
                        sample_period_us: 0,
                        buffered_samples: 0,
                    },
                );
                point
            }
        };
        let grant = state
            .scheduler
            .subscribe(point, 1, period_us, only_changes, now_us)
            .map_err(refusal_status)?;
        state.owners.insert(grant.id, Arc::clone(owner));
        self.wake.notify_all();
        Ok((grant.id, grant.period_us))
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

    pub(crate) fn stop(&self) {
        self.lock().stop = true;
        self.wake.notify_all();
    }

    pub(crate) fn run(&self, host: &Host, clock: &MonotonicClock) {
        let mut scratch = vec![0u8; 64 * 1024];
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
                    let result = host.call(point.device, point.method, &[], &mut scratch);
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
