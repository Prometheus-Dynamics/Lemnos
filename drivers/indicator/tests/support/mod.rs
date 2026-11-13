//! A `pixels/1` device on a fake bus, and the indicator bound to it the way a host binds it.
//! Each test binary uses a different part of this, so the rest is allowed to go unused.

#![allow(dead_code)]

use std::sync::Mutex;

use lemnos_core::{
    Config, ConfigValue, Decode, Dev, Encode, Health, Lifecycle, LocalCaller, MethodId, Reader,
    Result, Router, Status, Writer,
};
use lemnos_driver_indicator::{Indicator, IndicatorNeeds};

/// The fake's recorded state. Nothing grows: a frame is copied into a buffer sized at the start, so a
/// test can count allocations.
pub struct Inner {
    /// The frame set, not yet shown (4 bytes per LED).
    pub pending: Vec<u8>,
    /// The last frame shown (4 bytes per LED).
    pub shown: Vec<u8>,
    /// How many frames were shown.
    pub shows: usize,
    /// `show` calls, including failed ones.
    pub show_attempts: usize,
    /// How many of the next `show` calls fail.
    pub fail_show: usize,
}

/// A strip of `count` LEDs that records what it is sent.
pub struct FakePixels {
    count: usize,
    inner: Mutex<Inner>,
}

impl FakePixels {
    /// A fake for the life of the test (the driver borrows it for `'static`).
    pub fn leak(count: usize) -> &'static FakePixels {
        Box::leak(Box::new(FakePixels {
            count,
            inner: Mutex::new(Inner {
                pending: vec![0; count * 4],
                shown: vec![0; count * 4],
                shows: 0,
                show_attempts: 0,
                fail_show: 0,
            }),
        }))
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut self.inner.lock().unwrap())
    }

    /// The last frame shown, LED by LED.
    pub fn last(&self) -> Vec<[u8; 4]> {
        self.with(|s| {
            s.shown
                .chunks(4)
                .map(|c| [c[0], c[1], c[2], c[3]])
                .collect()
        })
    }

    pub fn shows(&self) -> usize {
        self.with(|s| s.shows)
    }

    pub fn show_attempts(&self) -> usize {
        self.with(|s| s.show_attempts)
    }

    pub fn fail_next_shows(&self, n: usize) {
        self.with(|s| s.fail_show = n);
    }
}

impl Router for FakePixels {
    fn call(&self, _device: u32, method: MethodId, args: &[u8], reply: &mut [u8]) -> Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        match method.method {
            // pixel_count
            1 => {
                let mut out = Writer::new(reply);
                (self.count as u16).encode(&mut out)?;
                Ok(out.len())
            }
            // set(offset, pixels)
            2 => {
                let mut from = Reader::new(args);
                let offset = u16::decode(&mut from)?;
                let bytes = <&[u8]>::decode(&mut from)?;
                let at = usize::from(offset) * 4;
                let len = bytes.len().min(inner.pending.len().saturating_sub(at));
                inner.pending[at..at + len].copy_from_slice(&bytes[..len]);
                Ok(0)
            }
            // show
            3 => {
                inner.show_attempts += 1;
                if inner.fail_show > 0 {
                    inner.fail_show -= 1;
                    return Err(Status::TIMED_OUT);
                }
                let Inner { pending, shown, .. } = &mut *inner;
                shown.copy_from_slice(pending);
                inner.shows += 1;
                Ok(0)
            }
            _ => Err(Status::NOT_FOUND),
        }
    }

    fn health(&self, _device: u32) -> Health {
        Health::Ok
    }
}

/// Milliseconds as the nanoseconds a host ticks with.
pub fn ms(ms: u64) -> u64 {
    ms * 1_000_000
}

/// `colour` on every LED.
pub fn led(r: u8, g: u8, b: u8) -> [u8; 4] {
    [r, g, b, 0]
}

/// The indicator started with `entries` as its config, on `fake`.
pub fn start(fake: &'static FakePixels, entries: &[(&str, ConfigValue<'_>)]) -> Result<Indicator> {
    let needs = IndicatorNeeds {
        pixels: Dev::new(LocalCaller::new(fake, 0, 0)),
    };
    <Indicator as Lifecycle>::start(needs, &Config::new(entries))
}

/// [`start`] for a configuration that must be accepted.
pub fn driver(fake: &'static FakePixels, entries: &[(&str, ConfigValue<'_>)]) -> Indicator {
    match start(fake, entries) {
        Ok(indicator) => indicator,
        Err(status) => panic!("the indicator was refused: {status:?}"),
    }
}

/// A strip of `count` LEDs at full brightness with no configuration of its own.
pub fn plain() -> Vec<(&'static str, ConfigValue<'static>)> {
    Vec::new()
}

/// Ticks `indicator` at `now_ms` and returns when it asks to be ticked next, in ms.
pub fn tick(indicator: &mut Indicator, now_ms: u64) -> Option<u64> {
    <Indicator as Lifecycle>::tick(indicator, ms(now_ms)).map(|ns| ns / 1_000_000)
}

/// Ticks from `now_ms` on the times the driver asks for, until it asks for none. Panics if it still
/// asks after `limit_ms`. Returns the time it stopped at.
pub fn settle(indicator: &mut Indicator, now_ms: u64, limit_ms: u64) -> u64 {
    let mut now = now_ms;
    let mut next = tick(indicator, now);
    while let Some(at) = next {
        assert!(at <= limit_ms, "still ticking at {at} ms");
        now = at;
        next = tick(indicator, now);
    }
    now
}
