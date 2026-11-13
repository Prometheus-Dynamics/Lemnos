//! A strip's frame model: a LED's colour, the geometry of a ring, and the bit stream an SPI bus
//! sends for a frame.
//!
//! Frames are in logical order. [`StripConfig::physical`] maps a logical index to the LED it lands
//! on, so index 0 is the ring's defined top whatever the wiring.

/// One LED's colour: red, green, blue and (for RGBW parts) white.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgbw {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub w: u8,
}

impl Rgbw {
    pub const fn new(r: u8, g: u8, b: u8, w: u8) -> Self {
        Self { r, g, b, w }
    }

    /// A colour from `0xRRGGBB` (white 0).
    pub const fn rgb(rgb: u32) -> Self {
        Self::new((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 0)
    }

    /// `0xRRGGBB`.
    pub const fn to_rgb(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }

    /// Every component scaled by `level / 255`, rounded to nearest.
    pub const fn scaled(self, level: u8) -> Self {
        const fn s(c: u8, level: u8) -> u8 {
            ((c as u16 * level as u16 + 127) / 255) as u8
        }
        Self::new(
            s(self.r, level),
            s(self.g, level),
            s(self.b, level),
            s(self.w, level),
        )
    }
}

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
    /// Bits per LED on the wire, the count a PIO machine shifts out of each word.
    pub const fn bits(self) -> u8 {
        match self {
            Self::Rgb => 24,
            Self::Rgbw => 32,
        }
    }

    /// Data bytes per LED on the wire.
    pub const fn bytes(self) -> usize {
        match self {
            Self::Rgb => 3,
            Self::Rgbw => 4,
        }
    }

    /// `"rgb"` or `"rgbw"` (`"grb"` and `"grbw"` are accepted too).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "rgb" | "grb" => Some(Self::Rgb),
            "rgbw" | "grbw" => Some(Self::Rgbw),
            _ => None,
        }
    }
}

/// Which way logical indices go round a ring, relative to the order the LEDs are wired in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Direction {
    /// Logical index `i + 1` is the next LED in wiring order (clockwise when the ring is wired
    /// clockwise).
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

/// A strip or ring: its geometry (count, the physical LED that is logical LED 0, direction), wire
/// format and brightness.
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

    /// Bytes of the logical frame: 4 per LED (r, g, b, w), whatever the wire format.
    pub const fn pixel_bytes(&self) -> usize {
        self.count as usize * 4
    }

    /// Bytes [`encode_spi`] writes: 3 SPI bytes for each data byte of each LED.
    pub const fn spi_len(&self) -> usize {
        self.count as usize * self.wire.bytes() * 3
    }

    /// The colour a logical LED gets: brightness applied, and on an RGB strip the white component
    /// folded into red, green and blue.
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

/// Encodes `pixels` (logical order; LEDs past the end are off, extras are ignored) as the strip's
/// bit stream into `out`, which must hold [`StripConfig::spi_len`] bytes. Returns the bytes
/// written.
pub fn encode_spi(
    config: &StripConfig,
    pixels: impl Iterator<Item = Rgbw>,
    out: &mut [u8],
) -> usize {
    let per_led = config.wire.bytes() * 3;
    let len = config.spi_len().min(out.len() / per_led * per_led);
    let off = encode_spi_byte(0);
    for chunk in out[..len].as_chunks_mut::<3>().0 {
        *chunk = off;
    }
    for (index, pixel) in pixels.take(config.count as usize).enumerate() {
        let p = config.output(pixel);
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

/// Words [`encode_pio`] writes: one per LED, then one padding word of zero bits.
pub const fn pio_words(config: &StripConfig) -> usize {
    config.count as usize + 1
}

/// Encodes `pixels` (logical order; LEDs past the end are off, extras are ignored) as the words a
/// PIO machine shifts out, into `out`, which must hold [`pio_words`] words. Each word is left
/// justified: green, red, blue and white (the white byte only on RGBW) in bits 31 to 0, first
/// byte most significant. The machine takes [`Wire::bits`] bits of each word. The last word is
/// zero: its bits end with the line low, so the line stays low through the reset. Returns the
/// words written.
pub fn encode_pio(
    config: &StripConfig,
    pixels: impl Iterator<Item = Rgbw>,
    out: &mut [u32],
) -> usize {
    let len = pio_words(config).min(out.len());
    out[..len].fill(0);
    for (index, pixel) in pixels.take(config.count as usize).enumerate() {
        let p = config.output(pixel);
        let at = config.physical(index as u16) as usize;
        if let Some(slot) = out.get_mut(at) {
            *slot =
                u32::from(p.g) << 24 | u32::from(p.r) << 16 | u32::from(p.b) << 8 | u32::from(p.w);
        }
    }
    len
}

/// One data byte as 24 SPI bits, MSB first: `110` for a 1, `100` for a 0. With the `spi-lut`
/// feature this reads a 256-entry table instead.
pub const fn encode_spi_byte(byte: u8) -> [u8; 3] {
    #[cfg(feature = "spi-lut")]
    {
        SPI_LUT[byte as usize]
    }
    #[cfg(not(feature = "spi-lut"))]
    {
        spi_byte_bits(byte)
    }
}

/// The bit expansion behind [`encode_spi_byte`] (the table is built from it).
const fn spi_byte_bits(byte: u8) -> [u8; 3] {
    let mut bits: u32 = 0;
    let mut i = 0;
    while i < 8 {
        let one = byte & (0x80 >> i) != 0;
        bits = (bits << 3) | if one { 0b110 } else { 0b100 };
        i += 1;
    }
    [(bits >> 16) as u8, (bits >> 8) as u8, bits as u8]
}

/// [`spi_byte_bits`] for every byte, built at compile time.
#[cfg(feature = "spi-lut")]
const SPI_LUT: [[u8; 3]; 256] = {
    let mut table = [[0u8; 3]; 256];
    let mut byte = 0;
    while byte < 256 {
        table[byte] = spi_byte_bits(byte as u8);
        byte += 1;
    }
    table
};

#[cfg(test)]
mod tests;
