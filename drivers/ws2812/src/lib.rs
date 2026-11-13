//! A WS2812, SK6812 or compatible addressable LED strip, as a `pixels/1` device. The strip hangs
//! on one of two transports, chosen by the name the board file gives its `bus`:
//!
//! - an SPI device (`spidevB.C`): the bus carries the strip's own bit stream, three SPI bits per
//!   data bit, so the bus runs at 2.4 MHz. The board file must set that clock; the SPI trait has no
//!   way to ask for it. Each frame is one transaction followed by an 80 µs reset.
//! - a PIO output (`pio-out:gpio=N`): a state machine on the pin runs the strip's program at
//!   800 kHz, and the frame is pushed to its FIFO. See [`PROGRAM`] for the instructions.
//!
//! In both, back-to-back frames do not run together: the reset gap is in every frame.
//!
//! `set` stores LEDs into the logical frame and sends nothing. `show` encodes the stored frame and
//! sends it. The buffers are allocated once, in `start`: the logical frame (4 bytes per LED), and
//! the encoded bit stream (SPI bytes or PIO words). Neither `set` nor `show` allocates.
//!
//! Failures are never final. A failed write is returned to the caller and health reports it. The
//! logical frame is kept, so the next `show` sends the same frame again. There is no retry timer and
//! nothing to back off.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

use lemnos_contract_pixels::Pixels as PixelsContract;
use lemnos_core::{
    BusKind, Config, Description, Health, Item, Lifecycle, MethodId, NeedKind, Pio, Resource,
    Resources, Result, Spi, Status, Unit, driver, status_of,
};
use lemnos_hal::pio::{PioJob, PioOperation};
use lemnos_hal::spi::{Operation, SpiDevice};

mod strip;

pub use strip::{
    Direction, Rgbw, StripConfig, Wire, encode_pio, encode_spi, encode_spi_byte, pio_words,
};

/// The reset a strip needs after a frame, in nanoseconds.
const RESET_NS: u32 = 80_000;

/// The PIO machine's clock: one bit is ten of its cycles, so 8 MHz gives 800 kHz.
const PIO_CLOCK_HZ: u32 = 8_000_000;
/// One bit on the wire, in nanoseconds (10 cycles at 8 MHz).
const BIT_NS: u32 = 1_250;
/// Words a PIO machine holds after the driver's last one has been handed over: four in its
/// transmit FIFO and one in its shift register. The reset gap is counted from the end of the
/// frame's padding word, which can be that many words later.
const PIO_WORDS_AHEAD: u32 = 5;

/// The strip's program, one word per instruction, relative to its first instruction. It uses
/// one side-set bit, which drives the data pin on every instruction, and the machine's wrap
/// window covers all four.
///
/// For each data bit (ten cycles, 1.25 µs):
/// - `out x, 1  side 0 [2]`: shift the next bit into X; the line is low for three cycles.
/// - `jmp !x, 3  side 1 [1]`: a zero bit (X clear) goes to the low tail; the line is high for two
///   cycles.
/// - `jmp 0  side 1 [4]`: a one bit, high for seven cycles in all, then back to the shift.
/// - `nop  side 0 [4]`: a zero bit's low tail, five cycles.
///
/// So a one is 875 ns high and 375 ns low, and a zero 250 ns high and 1000 ns low. Those are the
/// usual RP2040-family WS2812 timings; the datasheet's tolerances are about 150 ns either way on
/// the zero's high time, so check the strip on the bench.
///
/// Encodings: `out x, 1` is `0b011 side/delay 0b001 1`, a jump is `0b000`, `nop` is `mov y, y`.
pub const PROGRAM: [u16; 4] = [
    0x6221, // out x, 1   side 0 [2]
    0x1123, // jmp !x, 3  side 1 [1]
    0x1400, // jmp 0      side 1 [4]
    0xa442, // mov y, y   side 0 [4]   (nop)
];

/// The transport a strip hangs on: an SPI device or a PIO output, by the name the board file's
/// `bus` gives. A PIO output's name is `pio-out:gpio=N`; any other name is an SPI device.
pub enum Link {
    Spi(Spi),
    Pio(Pio),
}

impl Resource for Link {
    /// The board file's `bus` fills this need; the kind says only "bus", since either is one.
    const KIND: NeedKind = NeedKind::Bus(BusKind::Spi);

    fn take(res: &mut Resources<'_>, need: &str) -> Result<Self> {
        match res.pio(need) {
            Ok(machine) => Ok(Self::Pio(machine)),
            Err(Status::NOT_FOUND | Status::NOT_SUPPORTED) => res.spi(need).map(Self::Spi),
            Err(error) => Err(error),
        }
    }
}

/// `pixel_count`: method 1 of the first contract this driver provides.
const PIXEL_COUNT: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// A strip on an SPI device or a PIO output. Board-file config: `count` (required, at least 1),
/// `wire` (`"rgb"` or `"rgbw"`, default `"rgb"`), `offset` (default 0), `direction` (`"cw"` or
/// `"ccw"`, default `"cw"`), and `brightness` (0-255, default 255).
#[driver("ws2812", provides = [PixelsContract])]
pub struct Ws2812 {
    #[needs]
    spi: Link,
    config: StripConfig,
    /// The logical frame, 4 bytes per LED (r, g, b, w), kept between shows.
    pixels: Vec<u8>,
    /// The encoded bit stream `show` sends over SPI, sized to the strip. Empty on PIO.
    frame: Vec<u8>,
    /// The encoded words `show` pushes to a PIO machine, sized to the strip. Empty on SPI.
    words: Vec<u32>,
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

    /// Sends the encoded frame and its reset. On SPI, the reset is a delay in the same transaction.
    /// On PIO, the delay is the reset plus the time the words still queued after the hand-off can
    /// take, so the line is low for the whole reset before the next frame's first bit.
    fn write_frame(&mut self) -> Result<()> {
        let result = match &mut self.spi {
            Link::Spi(spi) => spi.transaction(&mut [
                Operation::Write(self.frame.as_slice()),
                Operation::DelayNs(RESET_NS),
            ]),
            Link::Pio(machine) => machine.transaction(&mut [
                PioOperation::Write(self.words.as_slice()),
                PioOperation::DelayNs(pio_gap_ns(self.config.wire)),
            ]),
        };
        self.failed = result.is_err();
        result.map_err(status_of)
    }
}

/// The delay a PIO frame asks for after its last word: the reset, plus the words that can still
/// be queued behind the padding word.
fn pio_gap_ns(wire: Wire) -> u32 {
    RESET_NS + PIO_WORDS_AHEAD * u32::from(wire.bits()) * BIT_NS
}

impl Lifecycle for Ws2812 {
    type Needs = Ws2812Needs;
    // Everything this driver does to hardware goes through its needs.
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;

    fn start(needs: Ws2812Needs, cfg: &Config<'_>) -> Result<Self> {
        let config = Self::strip_config(cfg)?;
        let (frame, words) = match needs.spi {
            Link::Spi(_) => (alloc::vec![0; config.spi_len()], Vec::new()),
            Link::Pio(_) => (Vec::new(), alloc::vec![0; pio_words(&config)]),
        };
        let mut strip = Self {
            spi: needs.spi,
            config,
            pixels: alloc::vec![0; config.pixel_bytes()],
            frame,
            words,
            failed: false,
        };
        if let Link::Pio(machine) = &mut strip.spi {
            // A machine that cannot take the program is a bind failure: nothing can be sent.
            let program = PioJob {
                program: &PROGRAM,
                wrap_bottom: 0,
                wrap_top: 3,
                clock_hz: PIO_CLOCK_HZ,
                out_bits: config.wire.bits(),
            };
            lemnos_hal::pio::PioMachine::load(&mut **machine, &program).map_err(status_of)?;
        }
        // Start with the strip off. A failed write here does not fail the bind: health reports it
        // and the next show tries again.
        strip.encode_off();
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

impl Ws2812 {
    /// Encodes an all-off frame into whichever buffer this transport sends.
    fn encode_off(&mut self) {
        match self.spi {
            Link::Spi(_) => {
                encode_spi(&self.config, core::iter::empty::<Rgbw>(), &mut self.frame);
            }
            Link::Pio(_) => {
                encode_pio(&self.config, core::iter::empty::<Rgbw>(), &mut self.words);
            }
        }
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
        match self.spi {
            Link::Spi(_) => {
                encode_spi(&self.config, leds, &mut self.frame);
            }
            Link::Pio(_) => {
                encode_pio(&self.config, leds, &mut self.words);
            }
        }
        self.write_frame()
    }
}
