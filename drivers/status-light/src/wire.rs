//! A look as the bytes a `light/1` `show` carries, and how it becomes a [`Look`].
//!
//! The first byte is the kind. A status look is its kind and one byte naming the status. A fill or a
//! comet carries its colour, its brightness and its envelope in full. Every multi-byte number is
//! little-endian. A look is at most [`MAX_LEN`] bytes, so it fits a local call. Anything else is
//! refused with `INVALID_ARGUMENT`.
//!
//! ```text
//! status:  1, status                                     (status: 0 ok, 1 warn, 2 error, 3 busy, 4 off)
//! fill:    2, r, g, b, w, brightness, envelope
//! comet:   3, r, g, b, w, brightness, period_ms u32, tail u16, heads u8, base u16, envelope
//! envelope: 0                                            solid
//!           1, period_ms u32, duty u16                   blink (duty in thousandths)
//!           2, period_ms u32, depth u16, easing u8       breathe (depth in thousandths)
//! ```
//!
//! Comet tails and bases are thousandths (the tail in LEDs, `1000` per LED); periods are in
//! milliseconds. Easing is an index into [`Easing::ALL`].

use lemnos_core::{Result, Status};

use crate::builtin::{self, StatusLook};
use crate::easing::Easing;
use crate::look::{Block, Envelope, Look, Rgbw, scale_level};
use crate::settings::Settings;

/// The longest look: a comet with a breathe.
pub const MAX_LEN: usize = 23;

const KIND_STATUS: u8 = 1;
const KIND_FILL: u8 = 2;
const KIND_COMET: u8 = 3;

const ENV_SOLID: u8 = 0;
const ENV_BLINK: u8 = 1;
const ENV_BREATHE: u8 = 2;

/// Periods a look may use, in milliseconds (the ranges the look files take).
const PERIOD_MS: core::ops::RangeInclusive<u32> = 100..=600_000;

/// A look as sent: a built-in status, or a fill or a comet given in full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spec {
    Status(StatusLook),
    Fill {
        color: Rgbw,
        brightness: u8,
        envelope: Envelope,
    },
    Comet {
        color: Rgbw,
        brightness: u8,
        period_ms: u32,
        tail: u16,
        heads: u8,
        base: u16,
        envelope: Envelope,
    },
}

impl Spec {
    /// Reads a look from `bytes`. Refused with `INVALID_ARGUMENT` when it is not a look, has bytes
    /// over, or has a value out of range.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut r = Reader { bytes, at: 0 };
        let spec = match r.byte()? {
            KIND_STATUS => Spec::Status(StatusLook::from_index(r.byte()?).ok_or(BAD)?),
            KIND_FILL => Spec::Fill {
                color: r.rgbw()?,
                brightness: r.byte()?,
                envelope: r.envelope()?,
            },
            KIND_COMET => {
                let color = r.rgbw()?;
                let brightness = r.byte()?;
                let period_ms = r.u32()?;
                let tail = r.u16()?;
                let heads = r.byte()?;
                let base = r.u16()?;
                Spec::Comet {
                    color,
                    brightness,
                    period_ms,
                    tail,
                    heads,
                    base,
                    envelope: r.envelope()?,
                }
            }
            _ => return Err(BAD),
        };
        if r.at != bytes.len() {
            return Err(BAD);
        }
        spec.check()?;
        Ok(spec)
    }

    /// Writes the look's bytes into `out`, and returns how many.
    pub fn encode(&self, out: &mut [u8; MAX_LEN]) -> usize {
        let mut w = Writer { out, at: 0 };
        match *self {
            Spec::Status(status) => {
                w.put(&[KIND_STATUS, status.index()]);
            }
            Spec::Fill {
                color,
                brightness,
                envelope,
            } => {
                w.put(&[KIND_FILL]);
                w.rgbw(color);
                w.put(&[brightness]);
                w.envelope(envelope);
            }
            Spec::Comet {
                color,
                brightness,
                period_ms,
                tail,
                heads,
                base,
                envelope,
            } => {
                w.put(&[KIND_COMET]);
                w.rgbw(color);
                w.put(&[brightness]);
                w.put(&period_ms.to_le_bytes());
                w.put(&tail.to_le_bytes());
                w.put(&[heads]);
                w.put(&base.to_le_bytes());
                w.envelope(envelope);
            }
        }
        w.at
    }

    /// The ranges a look may use (the ones a look file takes).
    fn check(&self) -> Result<()> {
        match *self {
            Spec::Status(_) => Ok(()),
            Spec::Fill { envelope, .. } => check_envelope(envelope),
            Spec::Comet {
                period_ms,
                tail,
                heads,
                base,
                envelope,
                ..
            } => {
                if !PERIOD_MS.contains(&period_ms)
                    || !(1..=2).contains(&heads)
                    || !(1..=64_000).contains(&tail)
                    || base > 1000
                {
                    return Err(BAD);
                }
                check_envelope(envelope)
            }
        }
    }
}

fn check_envelope(envelope: Envelope) -> Result<()> {
    match envelope {
        Envelope::Solid => Ok(()),
        Envelope::Blink { period_ms, duty } => {
            if PERIOD_MS.contains(&period_ms) && duty <= 1000 {
                Ok(())
            } else {
                Err(BAD)
            }
        }
        Envelope::Breathe {
            period_ms, depth, ..
        } => {
            if PERIOD_MS.contains(&period_ms) && depth <= 1000 {
                Ok(())
            } else {
                Err(BAD)
            }
        }
    }
}

/// The status for any look that is not a look.
const BAD: Status = Status::INVALID_ARGUMENT;

/// The look a [`Spec`] shows, from the board's `settings`: a built-in is made from them, and a fill
/// or a comet is scaled by the board's `look_brightness`.
pub fn resolve(spec: &Spec, s: &Settings) -> Look {
    match *spec {
        Spec::Status(status) => builtin::look(status, s),
        Spec::Fill {
            color,
            brightness,
            envelope,
        } => Look {
            block: Block::Fill { color },
            brightness: scale_level(brightness, s.look_brightness),
            envelope,
        },
        Spec::Comet {
            color,
            brightness,
            period_ms,
            tail,
            heads,
            base,
            envelope,
        } => Look {
            block: Block::Comet {
                color,
                period_ms,
                tail,
                heads,
                base,
            },
            brightness: scale_level(brightness, s.look_brightness),
            envelope,
        },
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self.at.checked_add(n).ok_or(BAD)?;
        let slice = self.bytes.get(self.at..end).ok_or(BAD)?;
        self.at = end;
        Ok(slice)
    }

    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn rgbw(&mut self) -> Result<Rgbw> {
        let b = self.take(4)?;
        Ok(Rgbw::new(b[0], b[1], b[2], b[3]))
    }

    fn envelope(&mut self) -> Result<Envelope> {
        Ok(match self.byte()? {
            ENV_SOLID => Envelope::Solid,
            ENV_BLINK => Envelope::Blink {
                period_ms: self.u32()?,
                duty: self.u16()?,
            },
            ENV_BREATHE => Envelope::Breathe {
                period_ms: self.u32()?,
                depth: self.u16()?,
                easing: Easing::from_index(self.byte()?).ok_or(BAD)?,
            },
            _ => return Err(BAD),
        })
    }
}

struct Writer<'a> {
    out: &'a mut [u8; MAX_LEN],
    at: usize,
}

impl Writer<'_> {
    fn put(&mut self, bytes: &[u8]) {
        self.out[self.at..self.at + bytes.len()].copy_from_slice(bytes);
        self.at += bytes.len();
    }

    fn rgbw(&mut self, c: Rgbw) {
        self.put(&[c.r, c.g, c.b, c.w]);
    }

    fn envelope(&mut self, envelope: Envelope) {
        match envelope {
            Envelope::Solid => self.put(&[ENV_SOLID]),
            Envelope::Blink { period_ms, duty } => {
                self.put(&[ENV_BLINK]);
                self.put(&period_ms.to_le_bytes());
                self.put(&duty.to_le_bytes());
            }
            Envelope::Breathe {
                period_ms,
                depth,
                easing,
            } => {
                self.put(&[ENV_BREATHE]);
                self.put(&period_ms.to_le_bytes());
                self.put(&depth.to_le_bytes());
                self.put(&[easing.index()]);
            }
        }
    }
}
