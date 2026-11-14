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
