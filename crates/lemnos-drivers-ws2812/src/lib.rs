//! WS2812 / SK6812 addressable LED strips, without `std` or allocation.
//!
//! A strip is a [`StripConfig`]: how many LEDs, the wire format
//! ([`Wire::Rgb`] for WS2812 and SK6812 RGB, [`Wire::Rgbw`] for SK6812
//! RGBW), the index offset (which physical LED is logical LED 0, for rings
//! mounted rotated), and a brightness. Frames are [`Rgbw`] slices in logical
//! order; the encoders turn a frame into what a transport sends:
//!
//! - [`encode_rp1`]: the Raspberry Pi RP1 `ws2812-pio` character device
//!   (`/dev/ledsN`): 4 bytes per LED, each a little-endian `u32` of
//!   `r | g << 8 | b << 16 | w << 24`. The kernel driver gamma-corrects and
//!   sends GRB or GRBW depending on its `rgbw` device-tree property, so the
//!   layout is the same for both wire formats.
//! - [`encode_spi`]: the strip's own bit stream for an SPI bus at 2.4 MHz,
//!   three SPI bits per data bit (`110` for 1, `100` for 0), GRB or GRBW, for
//!   microcontrollers without a PIO.
//!
//! `lemnos-drivers-linux`'s `Ws2812Pio` is the device-model LED strip over
//! the RP1 device; firmware drives the same frames over its SPI.

#![no_std]
#![forbid(unsafe_code)]

pub use lemnos_device::Rgbw;

#[cfg(test)]
mod tests;

/// The order and width of each LED's data on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Wire {
    /// Three bytes per LED, green-red-blue (WS2812, SK6812 RGB).
    #[default]
    Rgb,
    /// Four bytes per LED, green-red-blue-white (SK6812 RGBW).
    Rgbw,
}

impl Wire {
    /// Data bytes per LED on the wire.
    pub const fn bytes(self) -> usize {
        match self {
            Self::Rgb => 3,
            Self::Rgbw => 4,
        }
    }

    /// `"rgb"` or `"rgbw"`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "rgb" | "grb" => Some(Self::Rgb),
            "rgbw" | "grbw" => Some(Self::Rgbw),
            _ => None,
        }
    }
}

/// Which way logical indices go round a ring, relative to the order the
/// LEDs are wired in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Direction {
    /// Logical index `i + 1` is the next LED in wiring order (clockwise when
    /// the ring is wired clockwise).
    #[default]
    Cw,
    /// Logical index `i + 1` is the previous LED in wiring order.
    Ccw,
}

impl Direction {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "cw" | "clockwise" => Some(Self::Cw),
            "ccw" | "counterclockwise" | "anticlockwise" => Some(Self::Ccw),
            _ => None,
        }
    }
}

/// A strip or ring: its geometry (count, the physical LED that is logical
/// LED 0, direction), wire format and brightness. Frames are always in
/// logical order, so index 0 is the ring's defined "top" whatever the
/// wiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StripConfig {
    /// LEDs on the strip.
    pub count: u16,
    pub wire: Wire,
    /// The physical index of logical LED 0.
    pub offset: u16,
    pub direction: Direction,
    /// 0-255, applied to every frame.
    pub brightness: u8,
}

impl StripConfig {
    pub const fn new(count: u16, wire: Wire) -> Self {
        Self {
            count,
            wire,
            offset: 0,
            direction: Direction::Cw,
            brightness: 255,
        }
    }

    pub const fn with_direction(mut self, direction: Direction) -> Self {
        self.direction = direction;
        self
    }

    pub const fn with_offset(mut self, offset: u16) -> Self {
        self.offset = offset;
        self
    }

    pub const fn with_brightness(mut self, brightness: u8) -> Self {
        self.brightness = brightness;
        self
    }

    /// The physical LED that shows logical LED `index`.
    pub const fn physical(&self, index: u16) -> u16 {
        if self.count == 0 {
            return 0;
        }
        let count = self.count as u32;
        let step = index as u32 % count;
        let step = match self.direction {
            Direction::Cw => step,
            Direction::Ccw => (count - step) % count,
        };
        ((self.offset as u32 + step) % count) as u16
    }

    /// Bytes [`encode_rp1`] writes.
    pub const fn rp1_len(&self) -> usize {
        self.count as usize * RP1_BYTES_PER_LED
    }

    /// Bytes [`encode_spi`] writes.
    pub const fn spi_len(&self) -> usize {
        self.count as usize * self.wire.bytes() * 3
    }

    /// The colour a logical LED gets: brightness applied, and on an RGB
    /// strip the white component folded into red, green and blue.
    pub const fn output(&self, pixel: Rgbw) -> Rgbw {
        let p = pixel.scaled(self.brightness);
        match self.wire {
            Wire::Rgbw => p,
            Wire::Rgb => Rgbw::new(
                p.r.saturating_add(p.w),
                p.g.saturating_add(p.w),
                p.b.saturating_add(p.w),
                0,
            ),
        }
    }
}

/// Bytes per LED in the RP1 `ws2812-pio` userspace layout.
pub const RP1_BYTES_PER_LED: usize = 4;

/// Encodes `pixels` (logical order; LEDs past the end are off) for the RP1
/// `ws2812-pio` device into `out`, which must hold
/// [`StripConfig::rp1_len`] bytes. Returns the bytes written.
pub fn encode_rp1(config: &StripConfig, pixels: &[Rgbw], out: &mut [u8]) -> usize {
    let len = config
        .rp1_len()
        .min(out.len() / RP1_BYTES_PER_LED * RP1_BYTES_PER_LED);
    out[..len].fill(0);
    for (index, pixel) in pixels.iter().take(config.count as usize).enumerate() {
        let p = config.output(*pixel);
        let at = config.physical(index as u16) as usize * RP1_BYTES_PER_LED;
        if let Some(slot) = out.get_mut(at..at + RP1_BYTES_PER_LED) {
            slot.copy_from_slice(&[p.r, p.g, p.b, p.w]);
        }
    }
    len
}

/// Encodes `pixels` as the strip's bit stream for an SPI bus clocked at
/// 2.4 MHz into `out` ([`StripConfig::spi_len`] bytes). Returns the bytes
/// written. The bus must idle low and the caller must leave at least 80 µs
/// of low between frames (the reset time).
pub fn encode_spi(config: &StripConfig, pixels: &[Rgbw], out: &mut [u8]) -> usize {
    let per_led = config.wire.bytes() * 3;
    let len = config.spi_len().min(out.len() / per_led * per_led);
    let off = encode_spi_byte(0);
    for chunk in out[..len].as_chunks_mut::<3>().0 {
        *chunk = off;
    }
    for (index, pixel) in pixels.iter().take(config.count as usize).enumerate() {
        let p = config.output(*pixel);
        let at = config.physical(index as u16) as usize * per_led;
        let data = [p.g, p.r, p.b, p.w];
        for (i, byte) in data[..config.wire.bytes()].iter().enumerate() {
            if let Some(slot) = out.get_mut(at + i * 3..at + i * 3 + 3) {
                slot.copy_from_slice(&encode_spi_byte(*byte));
            }
        }
    }
    len
}

/// One data byte as 24 SPI bits, MSB first: `110` for a 1, `100` for a 0.
pub const fn encode_spi_byte(byte: u8) -> [u8; 3] {
    let mut bits: u32 = 0;
    let mut i = 0;
    while i < 8 {
        let one = byte & (0x80 >> i) != 0;
        bits = (bits << 3) | if one { 0b110 } else { 0b100 };
        i += 1;
    }
    [(bits >> 16) as u8, (bits >> 8) as u8, bits as u8]
}
