//! Keeps devices retrying and tells clients when one changes state, reason or health.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_core::{Health, HostClock, Retry};
use lemnos_host::{DeviceInfo, DeviceState, JobEvent, JobUpdate};
use lemnos_wire::{Frame, health, state};

use crate::Core;
use crate::notify::Notifier;

const MAX_SLEEP: Duration = Duration::from_millis(250);
/// How often every device's health is looked at, when nothing else woke the thread. A change of
/// health reaches clients within this, and a driver whose health flaps sends at most one event
/// per look.
const LOOK_EVERY: Duration = Duration::from_millis(250);

/// A device's state as the wire carries it: its state, and its health and reason with it.
pub(crate) fn wire_state(info: &DeviceInfo, now_ns: u64) -> (u8, u8, String) {
    wire_of(&info.state, info.retry, &info.health, now_ns)
}

/// The most of a reason's text the wire carries: a longer one is cut and marked with `...`.
const MAX_REASON: usize = 160;

/// A retry as the reason reads: how many attempts have failed, why the last one did, and how
/// long until the next. Attempt 0 is a device switched on, which has no attempt to show.
pub(crate) fn retry_text(attempts: u32, last: &str, next_ns: u64, now_ns: u64) -> String {
    if attempts == 0 {
        return last.to_owned();
    }
    let ms = next_ns.saturating_sub(now_ns) / 1_000_000;
    let last = if last.chars().count() > MAX_REASON {
        let cut: String = last.chars().take(MAX_REASON - 3).collect();
        format!("{cut}...")
    } else {
        last.to_owned()
    };
    format!("retrying: attempt {attempts}, last reason: {last}, next try in {ms} ms")
}

fn wire_of(
    device: &DeviceState,
    retry: Option<Retry>,
    health_now: &Health,
    now_ns: u64,
) -> (u8, u8, String) {
    let (state, mut reason) = match device {
        DeviceState::Running => match retry {
            Some(retry) => (
                state::RUNNING,
                retry_text(retry.attempts, retry.reason, retry.next_ns, now_ns),
            ),
            None => (state::RUNNING, String::new()),
        },
        DeviceState::Backoff {
            attempts,
            next_retry_ns,
            reason,
        } => (
            state::BACKOFF,
            retry_text(*attempts, reason, *next_retry_ns, now_ns),
        ),
        DeviceState::Off(why) => (state::OFF, format!("{why:?}").to_lowercase()),
    };
    let health = match health_now {
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

/// What a client was last told about a device: the revision of its state, and its health.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Told {
    revision: u64,
    health: Health,
}

/// Sends an event for each device whose state has moved, or whose health has changed, since the
/// client was last told about it. An event carries the state now, which is the state after every
/// change since then. It is not compared with the state last sent: a device that went down and
/// came back between two looks is back where it was, but a client that listed it in between holds
/// the state from that list, so it must be told.
///
/// A device whose driver is in a call is left for the next look: its health is not known until
/// the call returns. Reads and sends nothing for a device that has not changed, and allocates
/// only to send.
fn send_changes(core: &Core, told: &mut Vec<Option<Told>>) {
    told.resize(core.host.device_count(), None);
    for (index, told) in told.iter_mut().enumerate() {
        let Some((revision, health)) = core.host.look(index as u32) else {
            continue;
        };
        let now = Told { revision, health };
        if *told == Some(now) {
            continue;
        }
        let devices = core.host.devices();
        let Some(info) = devices.iter().find(|info| info.index == index as u32) else {
            continue;
        };
        let (state, health, reason) =
            wire_of(&info.state, info.retry, &health, core.clock.now_ns());
        let event = Frame::Event {
            device: info.index,
            state,
            health,
            reason: &reason,
        };
        for client in crate::lock(&core.clients).iter() {
            if client.wants_events() {
                client.event(&event);
            }
        }
        *told = Some(Told {
            revision: info.revision,
            health: now.health,
        });
    }
}

/// Sends what changed about long jobs to whoever started each. A job whose starter has gone
/// has nobody to tell.
fn send_jobs(core: &Core, updates: &mut Vec<JobUpdate>) {
    core.host.take_job_updates(updates);
    if updates.is_empty() {
        return;
    }
    let clients = crate::lock(&core.clients);
    for update in updates.drain(..) {
        let Some(client) = clients.iter().find(|client| client.id == update.owner) else {
            continue;
        };
        match &update.event {
            JobEvent::Progress { done, total } => client.value(&Frame::JobProgress {
                job: update.job,
                done: *done,
                total: *total,
            }),
            JobEvent::Finished { status, result } => client.event(&Frame::JobDone {
                job: update.job,
                status: status.0,
                bytes: result,
            }),
        }
    }
}

pub(crate) fn run(core: &Arc<Core>, notifier: &Notifier) {
    let mut seen = core.host.changes();
    let mut told = Vec::new();
    let mut updates = Vec::new();
    send_changes(core, &mut told);
    let mut next_look = Instant::now() + LOOK_EVERY;
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
        // Read again: the tick may itself have changed something. Health is looked at only on
        // the look interval, not on every wake, since a driver's health costs a call into it.
        let changes = core.host.changes();
        let looking = Instant::now() >= next_look;
        if changes != seen || looking {
            seen = changes;
            if looking {
                next_look = Instant::now() + LOOK_EVERY;
            }
            send_changes(core, &mut told);
        }
        send_jobs(core, &mut updates);
        if let Some(interval) = watchdog
            && Instant::now() >= next_ping
        {
            notifier.ping();
            next_ping = Instant::now() + interval;
        }
        // The pass is finished: the loop is turning, as far as the liveness is concerned.
        core.liveness.turned();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retry_reads_its_attempts_reason_and_wait() {
        assert_eq!(
            retry_text(
                3,
                "the controller is not there",
                5_000_000_000,
                3_000_000_000
            ),
            "retrying: attempt 3, last reason: the controller is not there, next try in 2000 ms"
        );
        // A device switched on has no attempt to show: its reason is the word alone.
        assert_eq!(retry_text(0, "switched on", 9, 1), "switched on");
        // A wait already past reads as zero, never as a wrapped number.
        assert!(retry_text(1, "x", 1, 5).ends_with("next try in 0 ms"));
    }

    #[test]
    fn a_long_reason_is_cut_to_the_bound_and_marked() {
        let long = "x".repeat(400);
        let text = retry_text(2, &long, 0, 0);
        let cut = format!("{}...", "x".repeat(MAX_REASON - 3));
        assert!(
            text.contains(&format!("last reason: {cut}, next try")),
            "{text}"
        );
        assert!(text.len() < MAX_REASON + 64, "{}", text.len());
    }
}
