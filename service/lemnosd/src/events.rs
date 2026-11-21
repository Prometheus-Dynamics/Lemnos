//! Keeps devices retrying and tells clients when one changes state, reason or health.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lemnos_core::{Figures, Health, HostClock, NOTE_SLOTS, Note, Retry, Tail};
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
    wire_of(&info.state, info.retry, &info.health, &info.figures, now_ns)
}

/// Why a device waits, as the wire numbers it: the kind of its refusal, or none for a device that
/// is running or was just switched on. The numbers are [`lemnos_wire::refusal::Refusal`]'s.
pub(crate) fn wire_refusal(device: &DeviceState) -> lemnos_wire::refusal::Refusal {
    let kind = match device {
        DeviceState::Backoff { kind, .. } => *kind,
        _ => None,
    };
    lemnos_wire::refusal::Refusal::of(kind)
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

/// The health reason with the driver's measured figures after it, as `reason (name value, ...)`.
fn with_figures(text: &str, figures: &Figures) -> String {
    if figures.is_empty() {
        return text.to_string();
    }
    let shown: Vec<String> = figures
        .iter()
        .map(|(name, value)| format!("{name} {value}"))
        .collect();
    format!("{text} ({})", shown.join(", "))
}

fn wire_of(
    device: &DeviceState,
    retry: Option<Retry>,
    health_now: &Health,
    figures: &Figures,
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
            ..
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
                reason = with_figures(text, figures);
            }
            health::DEGRADED
        }
        Health::Failed(text) | Health::Gone(text) => {
            if reason.is_empty() {
                reason = with_figures(text, figures);
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
/// The line the daemon writes when a device first reaches `running ok` after a bind: its id and
/// how long after the bind the host first saw it running ok, in whole milliseconds. `ok_ns` is the
/// host's stamp of that moment (`Host::running_ok_ns`), not the time this thread looked.
fn running_line(id: &str, bound_ns: u64, ok_ns: u64) -> String {
    let ms = ok_ns.saturating_sub(bound_ns) / 1_000_000;
    format!("lemnosd: {id}: running ok {ms} ms after bind")
}

/// The line for one note a driver reported: its device, its text, and its number after a colon.
pub(crate) fn note_line(id: &str, note: Note) -> String {
    match note.value {
        Some(value) => format!("lemnosd: {id}: {}: {value}", note.text),
        None => format!("lemnosd: {id}: {}", note.text),
    }
}

/// The line for the notes a driver dropped because its ring was full when they were reported.
pub(crate) fn dropped_line(id: &str, dropped: u32) -> String {
    format!("lemnosd: {id}: {dropped} notes dropped before they were logged")
}

/// Logs the notes each device's driver has reported since the last look, in the order they were
/// reported, and how many were dropped. A driver in a call keeps its notes for the next look.
fn log_notes(core: &Core) {
    for index in 0..core.host.device_count() as u32 {
        // The notes are taken into a local array while the driver is locked, and the device's id is
        // looked up only once there is something to log.
        let mut held = [None; NOTE_SLOTS];
        let mut count = 0;
        let Some(dropped) = core.host.take_notes(index, &mut |note| {
            if let Some(slot) = held.get_mut(count) {
                *slot = Some(note);
                count += 1;
            }
        }) else {
            continue;
        };
        if count == 0 && dropped == 0 {
            continue;
        }
        let devices = core.host.devices();
        let Some(info) = devices.iter().find(|info| info.index == index) else {
            continue;
        };
        for note in held.iter().take(count).flatten() {
            core.log_line(&note_line(&info.id, *note));
        }
        if dropped > 0 {
            core.log_line(&dropped_line(&info.id, dropped));
        }
    }
}

fn send_changes(core: &Core, told: &mut Vec<Option<Told>>, announced: &mut Vec<Option<u64>>) {
    log_notes(core);
    told.resize(core.host.device_count(), None);
    announced.resize(core.host.device_count(), None);
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
        // Once per bind: a device that reaches `running ok` says how long its bring-up took.
        if health == Health::Ok
            && matches!(info.state, DeviceState::Running)
            && let Some(bound) = core.host.bound_ns(index as u32)
            && let Some(ok) = core.host.running_ok_ns(index as u32)
            && announced[index] != Some(bound)
        {
            announced[index] = Some(bound);
            let line = running_line(&info.id, bound, ok);
            core.log_line(&line);
        }
        let (state, health, reason) = wire_of(
            &info.state,
            info.retry,
            &health,
            &info.figures,
            core.clock.now_ns(),
        );
        let event = Frame::Event {
            device: info.index,
            state,
            health,
            reason: &reason,
            refusal: Tail(wire_refusal(&info.state).code()),
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
            JobEvent::Finished { status, result } => {
                // A job the host ended with a reason (its driver was lost) says it in the log too.
                if *status == lemnos_core::Status::INTERRUPTED && !result.is_empty() {
                    let devices = core.host.devices();
                    if let Some(info) = devices.iter().find(|info| info.index == update.device) {
                        core.log_line(&job_ended_line(&info.id, update.job, *status, result));
                    }
                }
                client.event(&Frame::JobDone {
                    job: update.job,
                    status: status.0,
                    bytes: result,
                });
            }
        }
    }
}

pub(crate) fn run(core: &Arc<Core>, notifier: &Notifier) {
    let mut seen = core.host.changes();
    let mut told = Vec::new();
    let mut announced = Vec::new();
    let mut updates = Vec::new();
    send_changes(core, &mut told, &mut announced);
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
            send_changes(core, &mut told, &mut announced);
        }
        send_jobs(core, &mut updates);
        if let Some(interval) = watchdog
            && Instant::now() >= next_ping
        {
            notifier.ping();
            next_ping = Instant::now() + interval;
        }
        // Every stream's readers: their limits, and the slots of readers that have gone. On every
        // pass, so a dead reader's holds are released within one pass (MAX_SLEEP at most).
        core.host.streams_tick();
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

#[cfg(test)]
mod running_line_tests {
    use super::running_line;

    /// A device that reaches `running ok` says its id and the milliseconds since its bind.
    #[test]
    fn the_bring_up_line_names_the_device_and_the_time_since_bind() {
        assert_eq!(
            running_line("imu", 2_000_000_000, 2_000_000_000 + 37_900_000),
            "lemnosd: imu: running ok 37 ms after bind"
        );
    }

    /// A clock that reads before the bind (the bind's own time is zero) never underflows.
    #[test]
    fn a_time_before_the_bind_reads_as_zero() {
        assert_eq!(
            running_line("mag", 5_000, 1),
            "lemnosd: mag: running ok 0 ms after bind"
        );
    }
}

#[cfg(test)]
mod figures_tests {
    use super::with_figures;
    use lemnos_core::Figures;

    /// The health reason with the measured figures after it, as `reason (name value, ...)`, and the
    /// reason alone when the driver measured none.
    #[test]
    fn a_reason_shows_its_figures_after_it_and_alone_when_there_are_none() {
        assert_eq!(with_figures("restarting", &Figures::NONE), "restarting");
        let figures = Figures::NONE
            .with("silent_ms", 300)
            .with("frame_length", 1742);
        assert_eq!(
            with_figures("no frame start: restarting the sensor", &figures),
            "no frame start: restarting the sensor (silent_ms 300, frame_length 1742)"
        );
    }
}

/// The log line for a job the host ended with a reason: which device, which job, and the words.
fn job_ended_line(device: &str, job: u64, status: lemnos_core::Status, reason: &[u8]) -> String {
    format!(
        "device {device}: job {job:#x} ended {}: {}",
        if status == lemnos_core::Status::INTERRUPTED {
            "interrupted"
        } else {
            "failed"
        },
        String::from_utf8_lossy(reason)
    )
}
