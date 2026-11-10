//! Keeps devices retrying and tells clients when one changes state.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_core::{Health, HostClock};
use lemnos_host::{DeviceInfo, DeviceState};
use lemnos_wire::{Frame, health, state};

use crate::Core;
use crate::notify::Notifier;

const MAX_SLEEP: Duration = Duration::from_millis(250);

/// A device's state as the wire carries it.
pub(crate) fn wire_state(info: &DeviceInfo) -> (u8, u8, String) {
    let (state, mut reason) = match &info.state {
        DeviceState::Running => (state::RUNNING, String::new()),
        DeviceState::Backoff { reason, .. } => (state::BACKOFF, reason.clone()),
        DeviceState::Off(why) => (state::OFF, format!("{why:?}").to_lowercase()),
    };
    let health = match &info.health {
        Health::Ok => health::OK,
        Health::Degraded(text) => {
            if reason.is_empty() {
                reason = (*text).to_string();
            }
            health::DEGRADED
        }
        Health::Failed(text) | Health::Gone(text) => {
            if reason.is_empty() {
                reason = (*text).to_string();
            }
            health::FAILED
        }
    };
    (state, health, reason)
}

fn send_changes(core: &Core, last: &mut Vec<(u8, u8, String)>) {
    let devices = core.host.devices();
    last.resize(devices.len(), (u8::MAX, u8::MAX, String::new()));
    for (info, last) in devices.iter().zip(last.iter_mut()) {
        let now = wire_state(info);
        if now == *last {
            continue;
        }
        let event = Frame::Event {
            device: info.index,
            state: now.0,
            health: now.1,
            reason: &now.2,
        };
        for client in crate::lock(&core.clients).iter() {
            if client.wants_events() {
                client.event(&event);
            }
        }
        *last = now;
    }
}

pub(crate) fn run(core: &Arc<Core>, notifier: &Notifier) {
    let mut seen = core.host.changes();
    let mut last = Vec::new();
    send_changes(core, &mut last);
    let watchdog = notifier.watchdog_interval();
    let mut next_ping = Instant::now();
    while !core.stopping() {
        let now = core.clock.now_ns();
        let retry = core.host.next_retry_ns();
        let changes = core.host.changes();
        if changes != seen || retry.is_some_and(|at| at <= now) {
            core.host.tick();
        } else if core.host.next_tick_ns().is_some_and(|at| at <= now) {
            core.host.run_ticks();
        }
        // Read again: the tick may itself have changed something.
        let changes = core.host.changes();
        if changes != seen {
            seen = changes;
            send_changes(core, &mut last);
        }
        if let Some(interval) = watchdog
            && Instant::now() >= next_ping
        {
            notifier.ping();
            next_ping = Instant::now() + interval;
        }
        // Read after the ticks, which is when a driver says when it wants the next one.
        let wake = match (retry, core.host.next_tick_ns()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let until_wake = wake.map_or(MAX_SLEEP, |at| {
            Duration::from_nanos(at.saturating_sub(core.clock.now_ns()))
        });
        core.wait_stop(until_wake.clamp(Duration::from_millis(1), MAX_SLEEP));
    }
}
