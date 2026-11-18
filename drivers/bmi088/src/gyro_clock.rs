//! The gyroscope's sample clock: when each sample was taken, on the host's clock, from the count
//! of samples its FIFO holds.
//!
//! The gyroscope has no sensor time, so its own oscillator is the only clock on the chip, and the
//! FIFO's fill is the only thing it reports about it. Each drain reads the fill between two host
//! clock readings. The count is exact: it is the samples drained before the read plus the ones the
//! FIFO held, and sample `n` is made when the count reaches `n + 1`.
//!
//! Two things are fitted, and they are separate:
//!
//! - The rate. A least-squares line from the count to the midpoint of each bracket. The first 64
//!   reads are a plain least-squares fit (weight `1 / n`), and after that the reads are weighted
//!   as the accelerometer's are (exponential, about 64 reads). The line's slope is what the stamps
//!   are extrapolated with, so it is the one quantity that must be accurate over a long span: it
//!   settles to about 0.1 % in 64 reads, and to 0.02 % in about 200 (at 200 Hz, about 16 s and 50 s).
//! - The phase. The count does not say where in a sample period a read fell. A read at host time
//!   `h` with count `S` says the chip's count position `p` is in `[S, S + 1)`. The line gives the
//!   position `q(h)` of the same host time, so the line's error `e` (in samples) at that read is in
//!   `(q - S - 1.5, q - S - 0.5]` (the half is the fit's midpoint convention). A bracket of two
//!   host readings gives both ends, from `h0` and `h1`. The windows of the last `WINDOW` reads,
//!   all recomputed against the current line, intersect to a narrow window for `e`. That is the
//!   gyroscope's phase, as the accelerometer's is bounded around its length read.
//!
//! The windows are recomputed on every read because a stored window goes stale as the line moves:
//! the line's error at a read depends on the slope at the read's time, and an old window taken with an
//! older slope is not consistent with a new one. The last `WINDOW` reads are about 4 s at 200 Hz,
//! over which the slope's error is far under the phase's width. The drains are spread by a
//! golden-ratio offset that does not accumulate (see `Die::tick`), so the reads fall at different
//! fractions of a period, and the window narrows to the spread of those fractions and the bracket.
//!
//! The stamp of sample `n` is the line at `n + 1 + e`, where `e` is the middle of the window. The
//! model publishes only once it has converged: `CONVERGED_READS` reads (the slope is settled), and
//! a window within `PHASE_WIDTH_NS`. A stamp is never earlier than the last one published. An
//! overflow of the FIFO (the count is lost) and a restart of the die restart the model, and the
//! restart is reported by name.

use crate::sensor_clock::{FORGET_BATCHES, Fit};

/// The reads whose windows are intersected: about 4 s at 200 Hz.
const WINDOW: usize = 16;
/// The reads before the model publishes: the slope has settled to about 0.1 % (see the module
/// doc). At 200 Hz a drain is about every 250 ms, so this is about 16 s.
pub(crate) const CONVERGED_READS: u32 = 64;

/// A read of the FIFO's fill: its count, and the host times of the bracket around it.
#[derive(Debug, Clone, Copy, Default)]
struct Read {
    count: u64,
    h0: u64,
    h1: u64,
}

/// The line the stamps are taken from, as it was when the last read was observed: the origin's
/// count and host time, and the fit's means and slope.
#[derive(Debug, Clone, Copy)]
struct Line {
    c0: u64,
    o0: u64,
    mx: f64,
    my: f64,
    slope: f64,
}

impl Line {
    /// The line's position (in samples from the origin's count) at host time `h`.
    fn position(&self, h: u64) -> f64 {
        self.mx + (h as f64 - self.o0 as f64 - self.my) / self.slope
    }
}

/// The window one read gives for the line's error `e`, in samples: `(q - s - 1.5, q - s - 0.5]` at
/// each end of its bracket, `q` being the line's position at that host time.
fn line_window(line: &Line, read: &Read) -> (f64, f64) {
    let s = read.count as f64 - line.c0 as f64;
    let lo = line.position(read.h0) - s - 1.5;
    let hi = line.position(read.h1) - s - 0.5;
    (lo, hi)
}

/// The gyroscope's model. Constant size, no allocation.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct GyroClock {
    /// The count and host time (the midpoint of the bracket) of the first bracketed read since the
    /// restart: the origin the line is taken from.
    origin: Option<(u64, u64)>,
    fit: Fit,
    /// Reads observed with a bracket since the restart, outliers included: the convergence counts
    /// them.
    batches: u32,
    /// Reads taken into the fit and the window: the fit's weights count these.
    accepted: u32,
    /// The last `WINDOW` reads, the newest at `next - 1`.
    reads: [Read; WINDOW],
    filled: usize,
    next: usize,
    /// The line and the phase window (the line's error `e`, in samples) as of the last read.
    line: Option<Line>,
    window: Option<(f64, f64)>,
    /// The last stamp published: no later stamp is earlier.
    floor: u64,
    /// Reads in a row that disagreed with the window.
    rejected: u32,
    /// The model has converged once since the restart. It stays converged: a window that widens
    /// afterwards does not stop the stamps, which would drop the samples the ring overwrites.
    locked: bool,
}

impl GyroClock {
    /// Forgets the model, and keeps the last stamp published so no later stamp is earlier.
    pub(crate) fn restart(&mut self) {
        let floor = self.floor;
        *self = Self {
            floor,
            ..Self::default()
        };
    }

    /// A read of the FIFO's fill that found `count` samples made, between host times `h0` and `h1`
    /// (`h0 <= h1`, a bracket the caller has already checked). The caller observes only a read whose
    /// drain had no loss, so the count is exact.
    ///
    /// Once the line exists, a read whose window does not overlap the window of the reads held is an
    /// outlier (its host reading was off), and it is left out of the fit and the windows. Sixteen
    /// reads in a row that disagree restart the reads held from the newest one: the line has moved,
    /// and the reads held are the stale ones.
    pub(crate) fn observe(&mut self, count: u64, h0: u64, h1: u64) {
        self.batches = self.batches.saturating_add(1);
        let (c0, o0) = *self.origin.get_or_insert((count, h0 + (h1 - h0) / 2));
        let s = count as f64 - c0 as f64;
        let hm = h0 as f64 + (h1 - h0) as f64 / 2.0 - o0 as f64;
        let read = Read { count, h0, h1 };
        let Some(slope) = self.fit.slope() else {
            self.accept(read, hm, s);
            return;
        };
        let line = Line {
            c0,
            o0,
            mx: self.fit.mx,
            my: self.fit.my,
            slope,
        };
        let fresh = line_window(&line, &read);
        let agrees = match self.intersect(&line) {
            Some((lo, hi)) => fresh.0 < hi && fresh.1 > lo,
            None => true,
        };
        if agrees {
            self.rejected = 0;
            self.accept(read, hm, s);
        } else {
            self.rejected += 1;
            if self.rejected >= WINDOW as u32 {
                // The line moved under the reads held: they are stale, so start again from this one.
                self.filled = 0;
                self.next = 0;
                self.rejected = 0;
                self.accept(read, hm, s);
            }
        }
        if let Some(line) = self.current_line() {
            // The window of the reads held, or, when the line has moved enough that they no longer
            // agree, the newest read's own window: wider, and the line still publishes.
            let newest = self.reads[(self.next + WINDOW - 1) % WINDOW];
            self.window = self
                .intersect(&line)
                .or_else(|| Some(line_window(&line, &newest)));
            self.line = Some(line);
        }
        if self.batches >= CONVERGED_READS && self.window.is_some() {
            self.locked = true;
        }
    }

    /// Takes a read into the fit and the window.
    fn accept(&mut self, read: Read, hm: f64, s: f64) {
        self.accepted = self.accepted.saturating_add(1);
        // The first reads are a plain least-squares fit (weight 1 / n), so a few points do not carry
        // an early slope error for long; after `FORGET_BATCHES` reads the weight is the forgetting one.
        let w = 1.0 / f64::from(self.accepted.min(FORGET_BATCHES as u32));
        self.fit.update_weighted(s + 0.5, hm, w);
        self.reads[self.next] = read;
        self.next = (self.next + 1) % WINDOW;
        self.filled = (self.filled + 1).min(WINDOW);
    }

    /// The line as the fit has it now, once it has a slope.
    fn current_line(&self) -> Option<Line> {
        let (c0, o0) = self.origin?;
        Some(Line {
            c0,
            o0,
            mx: self.fit.mx,
            my: self.fit.my,
            slope: self.fit.slope()?,
        })
    }

    /// The intersection of the windows of the reads held, against `line`. Empty only when there are
    /// no reads held.
    fn intersect(&self, line: &Line) -> Option<(f64, f64)> {
        let mut lo = f64::NEG_INFINITY;
        let mut hi = f64::INFINITY;
        for read in &self.reads[..self.filled] {
            let (l, h) = line_window(line, read);
            lo = lo.max(l);
            hi = hi.min(h);
        }
        (self.filled > 0 && lo < hi).then_some((lo, hi))
    }

    /// Whether the model is good enough to publish stamps.
    pub(crate) fn converged(&self) -> bool {
        self.locked && self.line.is_some() && self.window.is_some()
    }

    /// Sample `n`'s host time from the model, or `None` before it converges.
    pub(crate) fn stamp(&self, n: u64) -> Option<u64> {
        if !self.converged() {
            return None;
        }
        let line = self.line?;
        let (lo, hi) = self.window?;
        let e = (lo + hi) / 2.0;
        // Sample `n` is made when the count reaches `n + 1`: the position `n + 1 - c0`, and the
        // half sample of the fit's x, and the line's error.
        let x = (n as f64 + 1.0) - line.c0 as f64 + 0.5 + e;
        let at = line.o0 as f64 + line.my + line.slope * (x - line.mx);
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
