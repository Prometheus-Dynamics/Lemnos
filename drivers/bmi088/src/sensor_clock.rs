//! The accelerometer's sample clock: when each sample was taken, on the host's clock, from the
//! chip's own sensor time.
//!
//! The chip counts sensor time (register `0x18`, three bytes, 39.0625 us per tick, wrapping at 2^24)
//! and makes a sample every `grid` ticks: at 200 Hz that is every 128 ticks. A stamp is therefore
//! a function of two things, both read on the host's side of the bus:
//!
//! - the phase: sample `n` was taken at sensor tick `phase + grid * n`. A read of the sensor time
//!   after the newest sample `m` gives `phase + grid * m + age`, with `age` in `[0, grid)` plus
//!   the time the FIFO read took, so the running minimum of `read - grid * m` is the phase plus the
//!   smallest age seen. Its error shrinks with the number of batches, and only the phase is taken
//!   that way.
//! - the rate: each read is paired with the host time at its middle (between two reads of the host
//!   clock around it). A least-squares line through the pairs maps sensor ticks to host time, with
//!   exponentially forgetting weights: O(1) state, no stored history, and it follows a slow drift
//!   of the oscillator without lag (a line is fitted exactly, whatever its history).
//!
//! The model is published only once it has converged, and a stamp is never earlier than the last
//! one published. A chip reset, a sensor time that jumps, or a FIFO overflow restarts the model
//! (`restart`), and the restart is reported by name.

/// Sensor ticks per second: one tick is 39.0625 us.
const TICKS_PER_S: i64 = 25_600;
/// One sensor tick, nominal, in host ns.
const TICK_NS: f64 = 39_062.5;
/// The sensor time's wrap: 24 bits.
const WRAP: u32 = 1 << 24;
const WRAP_MASK: u32 = WRAP - 1;

/// A read is paired with host time only when the two readings around it are at most this far
/// apart. A wider bracket means the host was preempted inside the read, so its time is not used.
pub(crate) const MAX_BRACKET_NS: u64 = 2_000_000;
/// Host-timed batches (ticks that read the sensor time with a clean bracket) before the model
/// publishes. The phase is the minimum over the reads, which is within about one sample period
/// over this many of the true phase (under 0.3 ms at 200 Hz), and the rate's fit has this many
/// points. At 200 Hz a batch is 365 ms, so this is about 6 s.
pub(crate) const BATCHES: u32 = 16;
/// Each read's weight is `1 / FORGET_BATCHES`: the fit remembers about this many reads, about
/// 23 s at 200 Hz, short enough to follow the oscillator as it drifts with temperature.
const FORGET_BATCHES: f64 = 64.0;
/// A jump is a sensor time step whose host time disagrees by more than this, or by a tenth of
/// the step, whichever is more. Host jitter of a few ms on a 365 ms batch is never a jump.
const JUMP_FLOOR_NS: f64 = 2_000_000.0;
const JUMP_FRACTION: f64 = 0.10;

/// Why the model restarted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    /// The sensor time went back: a chip reset (a step of half a wrap or more).
    Reset,
    /// The sensor time went forward by an amount the host clock does not agree with.
    Jump,
}

/// Exponentially weighted least squares over `(x, y)`, in O(1) state. Means and the variance and
/// covariance about them are kept, each updated with the same weight, so a point on a line keeps
/// the line exact whatever came before it.
#[derive(Debug, Clone, Copy, Default)]
struct Fit {
    count: u32,
    mx: f64,
    my: f64,
    sxx: f64,
    sxy: f64,
}

impl Fit {
    fn update(&mut self, x: f64, y: f64) {
        if self.count == 0 {
            self.count = 1;
            self.mx = x;
            self.my = y;
            return;
        }
        self.count = self.count.saturating_add(1);
        let w = 1.0 / FORGET_BATCHES;
        let keep = 1.0 - w;
        let (dx, dy) = (x - self.mx, y - self.my);
        self.mx += w * dx;
        self.my += w * dy;
        self.sxx = keep * (self.sxx + w * dx * dx);
        self.sxy = keep * (self.sxy + w * dx * dy);
    }

    /// The slope, once the points spread along `x`.
    fn slope(&self) -> Option<f64> {
        (self.count >= 2 && self.sxx > 1e-9).then(|| self.sxy / self.sxx)
    }
}

/// The accelerometer's sample clock. Constant size, no allocation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SensorClock {
    /// Sensor ticks per sample.
    grid: i64,
    /// The last sensor time read (24 bits) and the unwrapped tick count it stands for.
    last_raw: Option<u32>,
    ticks: u64,
    /// The sensor ticks and host time of the last read that had host time.
    last_host: Option<(u64, u64)>,
    /// The phase: the least sensor tick any batch's newest sample could have, given its read.
    phase: Option<i64>,
    /// Host-timed batches since the last restart.
    batches: u32,
    /// The sensor ticks and host time of the first host-timed read since the last restart: the
    /// origin the fit is taken from.
    origin: Option<(u64, u64)>,
    fit: Fit,
    /// The last stamp published: no later stamp is earlier.
    floor: u64,
}

impl SensorClock {
    /// A clock for samples `period_ns` apart.
    pub(crate) fn new(period_ns: u64) -> Self {
        let grid = (period_ns as i64).saturating_mul(TICKS_PER_S) / 1_000_000_000;
        Self {
            grid: grid.max(1),
            last_raw: None,
            ticks: 0,
            last_host: None,
            phase: None,
            batches: 0,
            origin: None,
            fit: Fit::default(),
            floor: 0,
        }
    }

    /// Forgets the model (the phase and the fit), and keeps what must stay monotonic: the sensor
    /// time's unwrapping and the last stamp published.
    pub(crate) fn restart(&mut self) {
        self.last_host = None;
        self.phase = None;
        self.batches = 0;
        self.origin = None;
        self.fit = Fit::default();
    }

    /// Reads the chip's sensor time, `raw`, after the newest sample `newest` (`None` before any
    /// sample), and the host time of the read if it has one. Returns the restart it caused, if any.
    pub(crate) fn observe(
        &mut self,
        raw: u32,
        newest: Option<u64>,
        host: Option<u64>,
    ) -> Option<Event> {
        let raw = raw & WRAP_MASK;
        let mut event = None;
        match self.last_raw {
            None => self.ticks = u64::from(raw),
            Some(last) => {
                let step = raw.wrapping_sub(last) & WRAP_MASK;
                if step >= WRAP / 2 {
                    event = Some(Event::Reset);
                    self.ticks = u64::from(raw);
                } else {
                    self.ticks += u64::from(step);
                }
            }
        }
        self.last_raw = Some(raw);
        if event.is_none()
            && let (Some((t0, h0)), Some(h1)) = (self.last_host, host)
        {
            let expect = (self.ticks - t0) as f64 * TICK_NS;
            let actual = h1 as f64 - h0 as f64;
            if (actual - expect).abs() > JUMP_FLOOR_NS.max(JUMP_FRACTION * expect.abs()) {
                event = Some(Event::Jump);
            }
        }
        if event.is_some() {
            self.restart();
        }
        if let Some(newest) = newest {
            let candidate = self.ticks as i64 - self.grid * newest as i64;
            self.phase = Some(self.phase.map_or(candidate, |p| p.min(candidate)));
        }
        if let Some(h) = host {
            let (t0, h0) = *self.origin.get_or_insert((self.ticks, h));
            let x = (self.ticks as i64 - t0 as i64) as f64;
            let y = (h as i64 - h0 as i64) as f64;
            self.fit.update(x, y);
            self.batches = self.batches.saturating_add(1);
            self.last_host = Some((self.ticks, h));
        }
        event
    }

    /// Whether the model is good enough to publish stamps.
    pub(crate) fn converged(&self) -> bool {
        self.batches >= BATCHES && self.fit.slope().is_some() && self.phase.is_some()
    }

    /// Sample `n`'s host time from the model, or `None` before it converges.
    pub(crate) fn stamp(&self, n: u64) -> Option<u64> {
        if !self.converged() {
            return None;
        }
        let phase = self.phase?;
        let (t0, h0) = self.origin?;
        let slope = self.fit.slope()?;
        let x = (phase + self.grid * n as i64 - t0 as i64) as f64;
        let at = h0 as f64 + self.fit.my + slope * (x - self.fit.mx);
        Some(if at <= 0.0 { 0 } else { (at + 0.5) as u64 })
    }

    /// The stamp to publish for sample `n`: the model's, and never earlier than the last one
    /// published. `None` before the model converges.
    pub(crate) fn publish(&mut self, n: u64) -> Option<u64> {
        let at = self.stamp(n)?.max(self.floor);
        self.floor = at;
        Some(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fit_of_a_line_is_the_line_whatever_came_before() {
        let mut fit = Fit::default();
        for x in [0.0, 3.0, 4.0, 10.0, 11.0] {
            fit.update(x, 2.0 * x + 5.0);
        }
        let slope = fit.slope().unwrap();
        assert!((slope - 2.0).abs() < 1e-9, "slope {slope}");
        let at = fit.my + slope * (20.0 - fit.mx);
        assert!((at - 45.0).abs() < 1e-6, "the line at 20 is {at}");
    }

    #[test]
    fn the_grid_is_the_sensor_ticks_per_sample_for_each_rate() {
        assert_eq!(SensorClock::new(5_000_000).grid, 128);
        assert_eq!(SensorClock::new(80_000_000).grid, 2048);
        assert_eq!(SensorClock::new(625_000).grid, 16);
    }

    #[test]
    fn a_reset_is_a_step_back_of_half_a_wrap_or_more() {
        let mut clock = SensorClock::new(5_000_000);
        assert_eq!(clock.observe(1_000, Some(0), Some(0)), None);
        assert_eq!(clock.observe(1_500, Some(1), Some(19_531_250)), None);
        assert_eq!(
            clock.observe(10, Some(2), Some(39_062_500)),
            Some(Event::Reset)
        );
    }
}
