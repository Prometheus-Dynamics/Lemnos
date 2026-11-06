//! The vendor kernel's multi-RGA driver, `/dev/rga` (RGA2 and RGA3 on vendor kernels): the route
//! for fill, mask ROP, blend, palette, colour key, gray input and turns that the mainline driver
//! lacks. Its request is `RGA_BLIT_SYNC` with `struct rga_req`, in `lemnos_linux_sys::rga`.
//!
//! The formats and enums here are the Apache-2.0 header's values (librga `rga.h` at commit
//! 331d5750, and `im2d_type.h` for the ROP and blend codes). Each operation's field mapping is
//! marked in its docs: "field mapping unverified" where the header does not state what a field
//! means, and nothing here has run on a board.

#[cfg(target_os = "linux")]
mod device;
#[allow(deprecated)]
pub use device::Librga;
#[cfg(target_os = "linux")]
pub use device::{Buffer, MultiRga};

/// A pixel format of [`Image`], with the value of `RK_FORMAT_*` in `rga.h` (Apache-2.0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum RgaFormat {
    /// R, G, B, A bytes.
    Rgba8888 = 0x0 << 8,
    /// R, G, B, X bytes: the fourth byte is ignored and the pixel is opaque.
    Rgbx8888 = 0x1 << 8,
    /// R, G, B bytes.
    Rgb888 = 0x2 << 8,
    /// B, G, R, A bytes.
    Bgra8888 = 0x3 << 8,
    /// B, G, R bytes.
    Bgr888 = 0x7 << 8,
    /// Two planes, 4:2:0: Y, then interleaved chroma at half resolution (`RK_FORMAT_YCbCr_420_SP`
    /// in `rga.h`, whose chroma order is Cr:Cb; `YCrCb_420_SP` is the other order).
    Nv12 = 0xa << 8,
    /// Y only (`RK_FORMAT_YCbCr_400`).
    Gray8 = 0x15 << 8,
    /// 8-bit palette indices (`RK_FORMAT_BPP8`).
    Bpp8 = 0x13 << 8,
}

impl RgaFormat {
    /// The value the request carries.
    pub fn code(self) -> i32 {
        self as i32
    }

    /// The bytes of `pixels` pixels in the frame (the chroma of NV12 included).
    pub fn frame_len(self, pixels: usize) -> usize {
        match self {
            Self::Rgba8888 | Self::Rgbx8888 | Self::Bgra8888 => pixels * 4,
            Self::Rgb888 | Self::Bgr888 => pixels * 3,
            Self::Nv12 => pixels * 3 / 2,
            Self::Gray8 | Self::Bpp8 => pixels,
        }
    }
}

/// An image's size and layout. Strides are in pixels, as the request counts them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image {
    pub format: RgaFormat,
    pub width: u32,
    pub height: u32,
    pub wstride: u32,
    pub hstride: u32,
}

impl Image {
    /// Tightly packed rows.
    pub fn packed(format: RgaFormat, width: u32, height: u32) -> Self {
        Self {
            format,
            width,
            height,
            wstride: width,
            hstride: height,
        }
    }

    /// The bytes the image spans.
    pub fn frame_len(&self) -> usize {
        self.format
            .frame_len(self.wstride as usize * self.hstride as usize)
    }
}

/// `dst = src op dst` (the `IM_ROP_*` codes of `im2d_type.h`, Apache-2.0). The request's ROP
/// table is not stated in the header: field mapping unverified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rop {
    And,
    Or,
    Xor,
}

impl Rop {
    /// The ROP3 code the request carries.
    pub fn code(self) -> i32 {
        match self {
            Rop::And => 0x88,
            Rop::Or => 0xee,
            Rop::Xor => 0xf6,
        }
    }
}

/// The Porter-Duff modes (`IM_ALPHA_BLEND_*`, `im2d_type.h`), foreground onto the destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendMode {
    /// The foreground over the destination.
    SrcOver,
    /// The foreground replaces the destination.
    Src,
    /// The destination over the foreground.
    DstOver,
}

impl BlendMode {
    /// The mode's `pd_mode` value. Field mapping unverified: the header names the field
    /// "porter duff alpha mode sel" but not the values; these are the crate's assumption and
    /// the board run must confirm them.
    pub fn pd_code(self) -> u8 {
        match self {
            BlendMode::SrcOver => 1,
            BlendMode::Src => 0,
            BlendMode::DstOver => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_frame_lengths_follow_the_headers() {
        assert_eq!(Image::packed(RgaFormat::Nv12, 4, 2).frame_len(), 12);
        assert_eq!(RgaFormat::Gray8.code(), 0x15 << 8);
        assert_eq!(RgaFormat::Bpp8.code(), 0x13 << 8);
        assert_eq!(Rop::Xor.code(), 0xf6);
        assert_eq!(Rop::And.code(), 0x88);
    }
}
