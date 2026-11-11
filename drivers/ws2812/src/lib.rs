//! A WS2812, SK6812 or compatible addressable LED strip on a SPI bus, as a `pixels/1` device.
//!
//! The bus carries the strip's own bit stream: three SPI bits per data bit, so the bus runs at
//! 2.4 MHz. The board file must set that clock; the SPI trait has no way to ask for it. Each frame
//! is sent in one transaction followed by an 80 µs reset, so the strip latches it and back-to-back
//! frames do not run together.
//!
//! `set` stores LEDs into the logical frame and sends nothing. `show` encodes the stored frame and
//! sends it. Two buffers are allocated once, in `start`: the logical frame (4 bytes per LED) and the
//! encoded bit stream. Neither `set` nor `show` allocates.
//!
//! Failures are never final. A failed write is returned to the caller and health reports it. The
//! logical frame is kept, so the next `show` sends the same frame again. There is no retry timer and
//! nothing to back off.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

use lemnos_contract_pixels::Pixels as PixelsContract;
use lemnos_core::{
    Config, Description, Health, Item, Lifecycle, MethodId, Result, Spi, Status, Unit, driver,
    status_of,
};
use lemnos_hal::spi::{Operation, SpiDevice};

mod strip;

pub use strip::{Direction, Rgbw, StripConfig, Wire, encode_spi, encode_spi_byte};

/// The reset a strip needs after a frame, in nanoseconds.
const RESET_NS: u32 = 80_000;

/// `pixel_count`: method 1 of the first contract this driver provides.
const PIXEL_COUNT: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// A strip on a SPI bus. Board-file config: `count` (required, at least 1), `wire` (`"rgb"` or
/// `"rgbw"`, default `"rgb"`), `offset` (default 0), `direction` (`"cw"` or `"ccw"`, default
/// `"cw"`), and `brightness` (0-255, default 255).
#[driver("ws2812", provides = [PixelsContract])]
pub struct Ws2812 {
    #[needs]
    spi: Spi,
    config: StripConfig,
    /// The logical frame, 4 bytes per LED (r, g, b, w), kept between shows.
    pixels: Vec<u8>,
    /// The encoded bit stream `show` sends, sized to the strip.
    frame: Vec<u8>,
    failed: bool,
}

impl Ws2812 {
    fn strip_config(cfg: &Config<'_>) -> Result<StripConfig> {
        let count = cfg
            .int("count")
            .and_then(|count| u16::try_from(count).ok())
            .filter(|&count| count > 0)
            .ok_or(Status::INVALID_ARGUMENT)?;
        let wire = match cfg.str("wire") {
            Some(name) => Wire::from_name(name).ok_or(Status::INVALID_ARGUMENT)?,
            None => Wire::Rgb,
        };
        let offset = match cfg.int("offset") {
            Some(offset) => u16::try_from(offset).map_err(|_| Status::INVALID_ARGUMENT)?,
            None => 0,
        };
        let direction = match cfg.str("direction") {
            Some(name) => Direction::from_name(name).ok_or(Status::INVALID_ARGUMENT)?,
            None => Direction::Cw,
        };
        let brightness = match cfg.int("brightness") {
            Some(level) => u8::try_from(level).map_err(|_| Status::INVALID_ARGUMENT)?,
            None => 255,
        };
        Ok(StripConfig::new(count, wire)
            .with_offset(offset)
            .with_direction(direction)
            .with_brightness(brightness))
    }

    /// Sends the encoded frame and its reset in one SPI transaction.
    fn write_frame(&mut self) -> Result<()> {
        let result = self.spi.transaction(&mut [
            Operation::Write(self.frame.as_slice()),
            Operation::DelayNs(RESET_NS),
        ]);
        self.failed = result.is_err();
        result.map_err(status_of)
    }
}

impl Lifecycle for Ws2812 {
    type Needs = Ws2812Needs;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;

    fn start(needs: Ws2812Needs, cfg: &Config<'_>) -> Result<Self> {
        let config = Self::strip_config(cfg)?;
        let mut strip = Self {
            spi: needs.spi,
            config,
            pixels: alloc::vec![0; config.pixel_bytes()],
            frame: alloc::vec![0; config.spi_len()],
            failed: false,
        };
        // Start with the strip off. A failed write here does not fail the bind: health reports it
        // and the next show tries again.
        encode_spi(&strip.config, core::iter::empty::<Rgbw>(), &mut strip.frame);
        let _ = strip.write_frame();
        Ok(strip)
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::value("pixels", PIXEL_COUNT).unit(Unit::One, 0));
    }

    fn health(&self) -> Health {
        if self.failed {
            return Health::Failed("the last SPI write failed; the next show retries");
        }
        Health::Ok
    }
}

impl PixelsContract for Ws2812 {
    fn pixel_count(&mut self) -> Result<u16> {
        Ok(self.config.count)
    }

    fn set(&mut self, offset: u16, pixels: &[u8]) -> Result<()> {
        let at = usize::from(offset).min(usize::from(self.config.count)) * 4;
        let room = self.pixels.len() - at;
        let len = pixels.len().min(room) / 4 * 4;
        self.pixels[at..at + len].copy_from_slice(&pixels[..len]);
        Ok(())
    }

    fn show(&mut self) -> Result<()> {
        let leds = self
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&[r, g, b, w]| Rgbw::new(r, g, b, w));
        encode_spi(&self.config, leds, &mut self.frame);
        self.write_frame()
    }
}
