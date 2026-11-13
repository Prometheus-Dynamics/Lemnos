//! The arithmetic of the driver: duty mapping, the PWM's duty time, the retry wait and the
//! tachometer. No I/O, no clock, no allocation.

use lemnos_hal::raw::Edge;

/// Full duty, per mille.
pub const FULL: u16 = 1000;
/// The first wait after a failed PWM write. Each further failure doubles it.
const RETRY_FIRST_NS: u64 = 1_000_000_000;
/// The longest wait between attempts to write the PWM.
pub const RETRY_CAP_NS: u64 = 8_000_000_000;
/// A tachometer with no pulse for this long (or for twice its pulse interval, when that is
/// longer) reads zero.
pub const STALL_NS: u64 = 1_000_000_000;
/// Nanoseconds per minute, for revolutions per minute.
const NS_PER_MINUTE: u128 = 60_000_000_000;

/// The duty the hardware gets for a requested `target`. Zero stays zero, so the fan can stop.
/// Any other request is raised to `min`, the lowest duty at which the fan turns.
pub fn hardware_duty(target: u16, min: u16) -> u16 {
    if target == 0 {
        0
    } else {
        target.max(min).min(FULL)
    }
}

/// The PWM's high time for a duty, in nanoseconds: `duty` of `period_ns`, rounded down.
pub fn duty_ns(period_ns: u64, duty: u16) -> u64 {
    let product = u128::from(period_ns) * u128::from(duty.min(FULL));
    (product / u128::from(FULL)) as u64
}

/// The PWM period for a frequency, in nanoseconds, rounded down.
pub fn period_ns(frequency_hz: u32) -> u64 {
    1_000_000_000 / u64::from(frequency_hz.max(1))
}

/// The wait after `attempt` failed writes in a row (`attempt` from 1): 1 s, 2 s, 4 s, then
/// 8 s for good.
pub fn backoff_ns(attempt: u32) -> u64 {
    let shift = attempt.saturating_sub(1).min(16);
    (RETRY_FIRST_NS << shift).min(RETRY_CAP_NS)
}

/// Revolutions per minute from one pulse interval: `pulses_per_rev` pulses make a revolution.
pub fn rpm_from_interval(interval_ns: u64, pulses_per_rev: u32) -> u32 {
    let per_rev = u128::from(interval_ns) * u128::from(pulses_per_rev.max(1));
    if per_rev == 0 {
        return 0;
    }
    (NS_PER_MINUTE / per_rev).min(u128::from(u32::MAX)) as u32
}

/// A tachometer's pulses, timed from their edges' own timestamps.
///
/// The revolutions come from the time between the last two rising edges. Each edge's sequence
/// number must follow the one before: a gap means edges were lost, and the interval across it is
/// not used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tach {
    pulses_per_rev: u32,
    last_seq: Option<u32>,
    last_rise_ns: Option<u64>,
    interval_ns: Option<u64>,
}

impl Tach {
    pub fn new(pulses_per_rev: u32) -> Self {
        Self {
            pulses_per_rev,
            last_seq: None,
            last_rise_ns: None,
            interval_ns: None,
        }
    }

    /// Takes one edge the line reported.
    pub fn edge(&mut self, edge: Edge) {
        let contiguous = self
            .last_seq
            .is_some_and(|seq| seq.wrapping_add(1) == edge.seq);
        self.last_seq = Some(edge.seq);
        if !contiguous {
            self.last_rise_ns = None;
            self.interval_ns = None;
        }
        if edge.rising {
            if let Some(previous) = self.last_rise_ns.filter(|&p| edge.timestamp_ns > p) {
                self.interval_ns = Some(edge.timestamp_ns - previous);
            }
            self.last_rise_ns = Some(edge.timestamp_ns);
        }
    }

    /// The revolutions per minute at `now_ns`, or zero when the pulses have stopped, or when two
    /// pulses have not yet been seen.
    pub fn rpm(&self, now_ns: u64) -> u32 {
        let (Some(last), Some(interval)) = (self.last_rise_ns, self.interval_ns) else {
            return 0;
        };
        let stalled_after = interval.saturating_mul(2).max(STALL_NS);
        if now_ns.saturating_sub(last) > stalled_after {
            return 0;
        }
        rpm_from_interval(interval, self.pulses_per_rev)
    }
}
