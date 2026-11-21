//! The picture every simulated frame shows, so the proof's reference file can be made from the
//! same function the front end's frames are.

/// The sensor mode's width.
pub const WIDTH: u32 = 1280;
/// The sensor mode's height.
pub const HEIGHT: u32 = 800;
/// Bytes of one packed RAW10 line.
pub const RAW10_STRIDE: usize = 1600;

/// The 10-bit pixel at column `x`, row `y`: a ramp that never repeats a line and never sits at zero.
#[must_use]
pub fn pixel(x: u32, y: u32) -> u16 {
    ((x * 3 + y * 5 + (x / 64) * 40) & 0x3ff) as u16 | 0x40
}

/// One frame as the sensor would send it, packed RAW10: four pixels in five bytes, the high eight
/// bits of each first and the four pairs of low bits last.
#[must_use]
pub fn reference_raw10() -> Vec<u8> {
    let mut out = Vec::with_capacity(RAW10_STRIDE * HEIGHT as usize);
    for y in 0..HEIGHT {
        for group in 0..WIDTH / 4 {
            let mut low = 0u8;
            for i in 0..4 {
                let p = pixel(group * 4 + i, y);
                out.push((p >> 2) as u8);
                low |= ((p & 3) as u8) << (2 * i);
            }
            out.push(low);
        }
    }
    out
}

/// The front end's output 0 for one frame: 16-bit little-endian pixels, the 10 bits moved up 6,
/// `stride` bytes a line, zero after the last line up to `size` bytes.
#[must_use]
pub fn frame16(stride: usize, size: usize) -> Vec<u8> {
    let mut out = vec![0u8; size];
    for y in 0..HEIGHT as usize {
        for x in 0..WIDTH as usize {
            let at = y * stride + x * 2;
            if let Some(slot) = out.get_mut(at..at + 2) {
                slot.copy_from_slice(&(pixel(x as u32, y as u32) << 6).to_le_bytes());
            }
        }
    }
    out
}

// ---------------------------------------------------------------- the instance scenes

/// The exposure a frame has when no control says otherwise: the reference, in microseconds.
pub const REFERENCE_EXPOSURE_US: u32 = 10_000;
/// The gain a frame has when no control says otherwise: 1x, in thousandths.
pub const REFERENCE_GAIN_MILLI: u32 = 1000;
/// The black level, in 10-bit codes: what a frame reads with no light.
pub const BLACK: u16 = 64;
/// The signal the pattern adds at the reference exposure and gain, in 10-bit codes, on the pattern's
/// mean level of one. At twice the reference the brightest rows clip, so a test stays below that.
pub const SIGNAL: f64 = 400.0;

/// What an instance's frames show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scene {
    /// A static ramp across the columns with bands every eight rows. The light is the exposure
    /// times the gain: the frame's mean is `BLACK + SIGNAL x exposure / reference x gain / reference`
    /// within rounding.
    Pattern,
    /// Packed RAW10 frames played back in turn: frame `n` is `frames[n % len]`. Exposure and gain
    /// do not change them.
    Replay(Vec<Vec<u8>>),
}

/// The pattern's level at column `x` of row `y`, on a mean of one over a whole frame.
#[must_use]
pub fn level(x: u32, y: u32) -> f64 {
    let ramp = 2.0 * (f64::from(x) + 0.5) / f64::from(WIDTH);
    let band = if (y / 8).is_multiple_of(2) { 0.9 } else { 1.1 };
    ramp * band
}

/// Packs one row of 10-bit codes (`values.len()` a multiple of four) as RAW10: four pixels in five
/// bytes, the high eight bits of each first and the low pairs last.
pub fn pack_row(values: &[u16], out: &mut [u8]) {
    for group in 0..values.len() / 4 {
        let px = &values[group * 4..group * 4 + 4];
        let mut low = 0u8;
        for (i, p) in px.iter().enumerate() {
            out[group * 5 + i] = (p >> 2) as u8;
            low |= ((p & 3) as u8) << (2 * i);
        }
        out[group * 5 + 4] = low;
    }
}

/// The pattern's frame at the given exposure and gain, as packed RAW10 into `out` (the frame's
/// size: `RAW10_STRIDE` a line, `HEIGHT` lines). Light is `exposure / reference x gain / reference`.
pub fn render_pattern(out: &mut [u8], exposure_us: u32, gain_milli: u32) {
    let scale = f64::from(exposure_us) / f64::from(REFERENCE_EXPOSURE_US) * f64::from(gain_milli)
        / f64::from(REFERENCE_GAIN_MILLI);
    let mut row = vec![0u16; WIDTH as usize];
    for y in 0..HEIGHT {
        for (x, code) in row.iter_mut().enumerate() {
            let v = f64::from(BLACK) + SIGNAL * scale * level(x as u32, y);
            *code = v.round().clamp(0.0, 1023.0) as u16;
        }
        let at = y as usize * RAW10_STRIDE;
        if let Some(dst) = out.get_mut(at..at + RAW10_STRIDE) {
            pack_row(&row, dst);
        }
    }
}

/// The bytes a paced frame with identity `id` carries: a function of the id and the byte's place,
/// so a reader can check any frame's bytes from its identity alone.
pub fn paced_byte(id: u64, at: usize) -> u8 {
    (at as u64)
        .wrapping_mul(31)
        .wrapping_add(id.wrapping_mul(131)) as u8
}

/// Fills `out` with the paced content of frame `id`.
pub fn fill_paced(id: u64, out: &mut [u8]) {
    for (at, b) in out.iter_mut().enumerate() {
        *b = paced_byte(id, at);
    }
}
