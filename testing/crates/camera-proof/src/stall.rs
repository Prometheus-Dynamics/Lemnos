//! What `lemnos-camera-proof --through-stall` makes of a stream reader's events.
//!
//! A reader that waits with `next_or_end` is told once when the producer (the camera, its
//! receiver, or anything that feeds it) fails and is being retried (`Stalled`), once when it is back
//! (`Resumed`, with `Kept` when the same driver instance recovered), and then gets frames. This
//! keeps the book of those events, prints one line for each with the `CLOCK_MONOTONIC` time it was
//! seen at, and says at the end whether every stall came back kept and with frames.
//!
//! The tracker does no I/O: the program feeds it the events and the clock, so a test can run the
//! same logic from a script.

/// What the reader saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A frame, with the sequence and timestamp (`CLOCK_MONOTONIC` ns) its producer gave it.
    Frame { sequence: u64, timestamp_ns: u64 },
    /// The producer failed. `since_ns` is when, on the producer's side, when the host said.
    Stalled { since_ns: Option<u64> },
    /// The producer is back; `kept` when the same instance recovered by itself.
    Resumed { kept: bool },
    /// The stream ended, with the reason in words.
    Ended(String),
}

/// How one stall went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stall {
    pub seen_ns: u64,
    pub since_ns: Option<u64>,
    pub last_sequence: Option<u64>,
    pub last_timestamp_ns: Option<u64>,
    pub resumed_ns: Option<u64>,
    pub kept: Option<bool>,
    pub first_sequence: Option<u64>,
    pub first_timestamp_ns: Option<u64>,
}

impl Stall {
    /// Whether the sequence went on from the last frame before the stall to the first after it:
    /// `Some(true)` for the next number, `Some(false)` for a jump, `None` while a frame is missing
    /// on either side.
    #[must_use]
    pub fn sequence_continued(&self) -> Option<bool> {
        Some(self.first_sequence? == self.last_sequence?.checked_add(1)?)
    }
}

/// The book of a run.
#[derive(Debug, Default)]
pub struct Tracker {
    stalls: Vec<Stall>,
    /// The frame before the next event, if any.
    last: Option<(u64, u64)>,
    ended: Option<String>,
    /// How many seconds a stall may last before the reader gives up on it.
    limit_ns: u64,
    gave_up: bool,
}

/// Milliseconds with microseconds, from nanoseconds.
fn ms(ns: u64) -> String {
    format!("{}.{:03} ms", ns / 1_000_000, ns / 1000 % 1000)
}

impl Tracker {
    /// A tracker that gives up on a stall after `limit_seconds`.
    #[must_use]
    pub fn new(limit_seconds: u64) -> Self {
        Self {
            limit_ns: limit_seconds.saturating_mul(1_000_000_000),
            ..Self::default()
        }
    }

    /// Whether a stall is open: told, and not yet resumed.
    #[must_use]
    pub fn stalled(&self) -> bool {
        self.stalls.last().is_some_and(|s| s.resumed_ns.is_none())
    }

    /// Takes one event seen at `now_ns`. Returns the line to print, if the event has one.
    pub fn event(&mut self, now_ns: u64, event: Event) -> Option<String> {
        match event {
            Event::Stalled { since_ns } => {
                let (last_sequence, last_timestamp_ns) = match self.last {
                    Some((s, t)) => (Some(s), Some(t)),
                    None => (None, None),
                };
                let line = format!(
                    "[{now_ns} ns] stalled: the producer is failing and being retried since {}; last frame before it: {}",
                    since_ns.map_or_else(|| "an unknown time".to_string(), |t| format!("{t} ns")),
                    match self.last {
                        Some((s, t)) => format!("sequence {s} timestamp {t} ns"),
                        None => "none".to_string(),
                    }
                );
                self.stalls.push(Stall {
                    seen_ns: now_ns,
                    since_ns,
                    last_sequence,
                    last_timestamp_ns,
                    resumed_ns: None,
                    kept: None,
                    first_sequence: None,
                    first_timestamp_ns: None,
                });
                Some(line)
            }
            Event::Resumed { kept } => {
                let stall = self.stalls.last_mut().filter(|s| s.resumed_ns.is_none())?;
                stall.resumed_ns = Some(now_ns);
                stall.kept = Some(kept);
                Some(format!(
                    "[{now_ns} ns] resumed ({}): {} after the stall was seen{}",
                    if kept {
                        "kept: the same driver instance recovered"
                    } else {
                        "lost: the host bound the device again, it must be configured and started again"
                    },
                    ms(now_ns.saturating_sub(stall.seen_ns)),
                    stall
                        .since_ns
                        .map_or_else(String::new, |t| format!(" (the failure was at {t} ns)")),
                ))
            }
            Event::Frame {
                sequence,
                timestamp_ns,
            } => {
                let line = self.stalls.last_mut().and_then(|stall| {
                    if stall.first_sequence.is_some() {
                        return None;
                    }
                    stall.first_sequence = Some(sequence);
                    stall.first_timestamp_ns = Some(timestamp_ns);
                    let since_stalled = ms(now_ns.saturating_sub(stall.seen_ns));
                    let since_resumed = stall
                        .resumed_ns
                        .map_or_else(|| "no resume seen".to_string(), |r| ms(now_ns.saturating_sub(r)));
                    let across = match stall.last_sequence {
                        Some(last) if sequence == last + 1 => {
                            format!("the sequence continued ({sequence} follows {last})")
                        }
                        Some(last) if sequence > last => format!(
                            "the sequence jumped from {last} to {sequence} ({} numbers skipped)",
                            sequence - last - 1
                        ),
                        Some(last) => {
                            format!("the sequence restarted ({sequence} after {last})")
                        }
                        None => "no frame before the stall to compare".to_string(),
                    };
                    Some(format!(
                        "[{now_ns} ns] first frame after the stall: sequence {sequence} timestamp {timestamp_ns} ns, {since_stalled} after stalled, {since_resumed} after resumed; {across}"
                    ))
                });
                self.last = Some((sequence, timestamp_ns));
                line
            }
            Event::Ended(kind) => {
                let line = format!("[{now_ns} ns] ended: {kind}");
                self.ended = Some(kind);
                Some(line)
            }
        }
    }

    /// A wait that returned with nothing, at `now_ns`. Returns true when a stall has lasted longer
    /// than the limit: the reader gives up. A silence with no stall open is the caller's to judge.
    pub fn idle(&mut self, now_ns: u64) -> bool {
        let over = self
            .stalls
            .last()
            .filter(|s| s.resumed_ns.is_none())
            .is_some_and(|s| now_ns.saturating_sub(s.seen_ns) > self.limit_ns);
        self.gave_up |= over;
        over
    }

    /// Stalls seen.
    #[must_use]
    pub fn stalls(&self) -> &[Stall] {
        &self.stalls
    }

    /// The longest stall in nanoseconds, from the moment it was seen to the resume (or `now_ns`
    /// for one still open).
    #[must_use]
    pub fn longest_ns(&self, now_ns: u64) -> u64 {
        self.stalls
            .iter()
            .map(|s| s.resumed_ns.unwrap_or(now_ns).saturating_sub(s.seen_ns))
            .max()
            .unwrap_or(0)
    }

    /// The summary lines: how many stalls, the longest, and for each whether the sequence
    /// continued or jumped across it.
    #[must_use]
    pub fn summary(&self, now_ns: u64) -> Vec<String> {
        let mut out = vec![format!(
            "stalls {}; longest stall {}",
            self.stalls.len(),
            ms(self.longest_ns(now_ns))
        )];
        for (i, stall) in self.stalls.iter().enumerate() {
            out.push(format!(
                "stall {i}: {}; {}; sequence {}",
                stall.resumed_ns.map_or_else(
                    || "never resumed".to_string(),
                    |r| format!("lasted {}", ms(r.saturating_sub(stall.seen_ns)))
                ),
                match stall.kept {
                    Some(true) => "kept",
                    Some(false) => "lost",
                    None => "not resumed",
                },
                match (stall.sequence_continued(), stall.first_sequence) {
                    (Some(true), _) => "continued across it".to_string(),
                    (Some(false), _) => format!(
                        "jumped across it ({} to {})",
                        stall.last_sequence.unwrap_or(0),
                        stall.first_sequence.unwrap_or(0)
                    ),
                    (None, None) => "no frame after it".to_string(),
                    (None, Some(_)) => "not comparable (no frame before it)".to_string(),
                },
            ));
        }
        out
    }

    /// Whether the run passes: every stall resumed `Kept` and frames came back after it, and the
    /// stream did not end. The error says why not.
    ///
    /// # Errors
    /// The first thing that failed, in words.
    pub fn verdict(&self) -> Result<(), String> {
        if let Some(kind) = &self.ended {
            return Err(format!("the stream ended: {kind}"));
        }
        if self.gave_up {
            return Err(format!(
                "a stall lasted longer than the limit of {} s and the reader gave up",
                self.limit_ns / 1_000_000_000
            ));
        }
        for (i, stall) in self.stalls.iter().enumerate() {
            match stall.kept {
                None => return Err(format!("stall {i} never resumed")),
                Some(false) => {
                    return Err(format!(
                        "stall {i} resumed lost: the device was bound again and must be started again"
                    ));
                }
                Some(true) => {}
            }
            if stall.first_sequence.is_none() {
                return Err(format!("stall {i} resumed and no frame came after it"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    fn frame(sequence: u64, timestamp_ns: u64) -> Event {
        Event::Frame {
            sequence,
            timestamp_ns,
        }
    }

    #[test]
    fn a_stall_that_resumes_kept_with_the_sequence_continuing_passes() {
        let mut t = Tracker::new(30);
        assert_eq!(t.event(S, frame(41, S - 5)), None);
        let stalled = t
            .event(
                2 * S,
                Event::Stalled {
                    since_ns: Some(S + 10),
                },
            )
            .unwrap();
        assert_eq!(
            stalled,
            "[2000000000 ns] stalled: the producer is failing and being retried since 1000000010 ns; last frame before it: sequence 41 timestamp 999999995 ns"
        );
        assert!(t.stalled());
        assert!(!t.idle(3 * S));
        let resumed = t
            .event(4 * S + 250_000, Event::Resumed { kept: true })
            .unwrap();
        assert_eq!(
            resumed,
            "[4000250000 ns] resumed (kept: the same driver instance recovered): 2000.250 ms after the stall was seen (the failure was at 1000000010 ns)"
        );
        let first = t
            .event(4 * S + 400_000_000, frame(42, 4 * S + 300_000_000))
            .unwrap();
        assert_eq!(
            first,
            "[4400000000 ns] first frame after the stall: sequence 42 timestamp 4300000000 ns, 2400.000 ms after stalled, 399.750 ms after resumed; the sequence continued (42 follows 41)"
        );
        assert_eq!(t.event(5 * S, frame(43, 5 * S)), None);
        assert_eq!(t.verdict(), Ok(()));
        let summary = t.summary(6 * S);
        assert_eq!(summary[0], "stalls 1; longest stall 2000.250 ms");
        assert_eq!(
            summary[1],
            "stall 0: lasted 2000.250 ms; kept; sequence continued across it"
        );
    }

    #[test]
    fn a_jump_across_a_stall_is_named_and_a_restart_too() {
        let mut t = Tracker::new(30);
        t.event(S, frame(10, S));
        t.event(2 * S, Event::Stalled { since_ns: None });
        t.event(3 * S, Event::Resumed { kept: true });
        let line = t.event(3 * S, frame(25, 3 * S)).unwrap();
        assert!(line.ends_with("the sequence jumped from 10 to 25 (14 numbers skipped)"));
        assert_eq!(t.stalls()[0].sequence_continued(), Some(false));
        assert_eq!(t.verdict(), Ok(()));
        t.event(4 * S, Event::Stalled { since_ns: None });
        t.event(5 * S, Event::Resumed { kept: true });
        let line = t.event(5 * S, frame(0, 5 * S)).unwrap();
        assert!(line.ends_with("the sequence restarted (0 after 25)"));
        assert_eq!(t.summary(6 * S)[0], "stalls 2; longest stall 1000.000 ms");
    }

    #[test]
    fn lost_never_resumed_ended_and_no_frame_each_fail_and_say_why() {
        let mut t = Tracker::new(30);
        t.event(S, frame(1, S));
        t.event(S, Event::Stalled { since_ns: None });
        assert_eq!(t.verdict(), Err("stall 0 never resumed".to_string()));
        t.event(2 * S, Event::Resumed { kept: false });
        t.event(2 * S, frame(0, 2 * S));
        assert!(t.verdict().unwrap_err().contains("resumed lost"));

        let mut t = Tracker::new(30);
        t.event(S, frame(1, S));
        t.event(S, Event::Stalled { since_ns: None });
        t.event(2 * S, Event::Resumed { kept: true });
        assert_eq!(
            t.verdict(),
            Err("stall 0 resumed and no frame came after it".to_string())
        );

        let mut t = Tracker::new(30);
        let line = t
            .event(S, Event::Ended("the device failed".to_string()))
            .unwrap();
        assert_eq!(line, "[1000000000 ns] ended: the device failed");
        assert_eq!(
            t.verdict(),
            Err("the stream ended: the device failed".to_string())
        );
    }

    #[test]
    fn a_stall_longer_than_the_limit_is_given_up_on_and_a_short_one_is_waited_through() {
        let mut t = Tracker::new(30);
        assert!(
            !t.idle(100 * S),
            "no stall open: not this tracker's to judge"
        );
        t.event(10 * S, Event::Stalled { since_ns: None });
        assert!(!t.idle(40 * S), "30 s is the limit itself");
        assert!(t.idle(40 * S + 1));
        assert!(t.verdict().unwrap_err().contains("limit of 30 s"));
    }

    #[test]
    fn a_resume_with_no_stall_open_says_nothing() {
        let mut t = Tracker::new(30);
        assert_eq!(t.event(S, Event::Resumed { kept: true }), None);
        assert_eq!(t.verdict(), Ok(()));
        assert_eq!(t.summary(S)[0], "stalls 0; longest stall 0.000 ms");
    }
}
