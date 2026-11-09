//! A `pixels/1` device on a fake bus, and the driver bound to it the way a host binds it.

use std::sync::Mutex;

use lemnos_core::{
    Config, ConfigValue, Decode, Dev, Encode, Health, LocalCaller, MethodId, Reader, Result,
    Router, Status, Writer,
};
use lemnos_driver_status_light::{Envelope, Rgbw, Spec, StatusLight, StatusLightNeeds};

/// The fake's recorded state.
#[derive(Default)]
pub struct Inner {
    /// The frame the driver has set, not yet shown (4 bytes per LED).
    pub pending: Vec<u8>,
    /// Every frame that was shown, in order.
    pub shows: Vec<Vec<u8>>,
    /// The offset of every `set` call that succeeded.
    pub set_offsets: Vec<u16>,
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
                ..Inner::default()
            }),
        }))
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut self.inner.lock().unwrap())
    }

    /// The last frame shown, LED by LED.
    pub fn last(&self) -> Vec<[u8; 4]> {
        self.with(|s| {
            let bytes = s.shows.last().cloned().unwrap_or_default();
            bytes.chunks(4).map(|c| [c[0], c[1], c[2], c[3]]).collect()
        })
    }

    pub fn shows(&self) -> usize {
        self.with(|s| s.shows.len())
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
                inner.set_offsets.push(offset);
                Ok(0)
            }
            // show
            3 => {
                inner.show_attempts += 1;
                if inner.fail_show > 0 {
                    inner.fail_show -= 1;
                    return Err(Status::TIMED_OUT);
                }
                let frame = inner.pending.clone();
                inner.shows.push(frame);
                Ok(0)
            }
            _ => Err(Status::NOT_FOUND),
        }
    }

    fn health(&self, _device: u32) -> Health {
        Health::Ok
    }
}

/// The driver started with `entries` as its config, on `fake`.
pub fn start(
    fake: &'static FakePixels,
    entries: &[(&str, ConfigValue<'_>)],
) -> Result<StatusLight> {
    let needs = StatusLightNeeds {
        pixels: Dev::new(LocalCaller::new(fake, 0, 0)),
    };
    <StatusLight as lemnos_core::Lifecycle>::start(needs, &Config::new(entries))
}

/// [`start`] for a configuration that must be accepted.
pub fn driver(fake: &'static FakePixels, entries: &[(&str, ConfigValue<'_>)]) -> StatusLight {
    match start(fake, entries) {
        Ok(light) => light,
        Err(status) => panic!("the light was refused: {status:?}"),
    }
}

/// A ring of `count` LEDs, fades off, at full look brightness: the simplest board to read frames
/// from.
pub fn plain(count: i64) -> Vec<(&'static str, ConfigValue<'static>)> {
    vec![
        ("count", ConfigValue::Int(count)),
        ("fade_ms", ConfigValue::Int(0)),
        ("look_brightness", ConfigValue::Float(1.0)),
    ]
}

/// Milliseconds as the nanoseconds a host ticks with.
pub fn ms(ms: u64) -> u64 {
    ms * 1_000_000
}

/// `colour` on every LED.
pub fn led(r: u8, g: u8, b: u8) -> [u8; 4] {
    [r, g, b, 0]
}

/// The look bytes for a fill at full brightness, steady.
pub fn fill(colour: u32) -> Vec<u8> {
    let mut out = [0u8; lemnos_driver_status_light::MAX_LEN];
    let n = Spec::Fill {
        color: Rgbw::rgb(colour),
        brightness: 255,
        envelope: Envelope::Solid,
    }
    .encode(&mut out);
    out[..n].to_vec()
}

/// The look bytes of `spec`.
pub fn bytes(spec: Spec) -> Vec<u8> {
    let mut out = [0u8; lemnos_driver_status_light::MAX_LEN];
    let n = spec.encode(&mut out);
    out[..n].to_vec()
}

/// Ticks `light` from `now_ms` on the times it asks for, until it asks for none, and returns the
/// time it stopped at. Panics if it is still asking after `limit_ms`.
pub fn settle(light: &mut StatusLight, now_ms: u64, limit_ms: u64) -> u64 {
    let mut now = now_ms;
    let mut next = <StatusLight as lemnos_core::Lifecycle>::tick(light, ms(now));
    while let Some(at) = next {
        assert!(
            at / 1_000_000 <= limit_ms,
            "still ticking at {}",
            at / 1_000_000
        );
        now = at / 1_000_000;
        next = <StatusLight as lemnos_core::Lifecycle>::tick(light, ms(now));
    }
    now
}
