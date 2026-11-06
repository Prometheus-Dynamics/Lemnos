//! The vendor kernel's multi-RGA request, `/dev/rga`: `RGA_BLIT_SYNC` and `struct rga_req`, as
//! the BSP RGA2 and RGA3 drivers take it.
//!
//! Copyright (C) 2016 Rockchip Electronics Co., Ltd. (authors: Zhiqin Wei, Yu Qiaowei), licensed
//! under the Apache License, Version 2.0. The structures are transcribed from `include/rga/rga.h`
//! of the librga release at commit 331d5750 (`release_version.txt`, version 1.8.1), and the
//! request number is `RGA_BLIT_SYNC` 0x5017 there and in `drmrga.h`. The C compiler's sizes and
//! offsets on LP64 are pinned by the tests below. No GPL kernel header was used.
//!
//! The request holds no pointers, only 64-bit addresses the kernel follows: a dma-buf
//! descriptor or a user address, depending on the buffer. A caller cannot write those
//! addresses directly: [`Job`] takes them from buffers it keeps borrowed until the call returns,
//! so the kernel reads and writes only memory the caller lent for that call.
//!
//! Field mappings the header does not spell out are marked in each item's docs as unverified.

use std::io;
use std::os::fd::BorrowedFd;

use crate::ioctl::ioctl_ptr;

/// `RGA_BLIT_SYNC`: the synchronous blit, which takes a [`RgaReq`].
pub const BLIT_SYNC: u32 = 0x5017;

/// `rga_img_info_t`: one image. `yrgb_addr` is the first plane's address; the rest are zero for
/// the packed formats used here.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImgInfo {
    /// First plane's address (set by [`Job`]).
    pub yrgb_addr: u64,
    /// Chroma plane's address (set by [`Job`] to zero; unused for packed formats).
    pub uv_addr: u64,
    /// Second chroma plane's address (zero for packed formats).
    pub v_addr: u64,
    /// `RK_FORMAT_*`, the surface format of the header (`RgaFormat` in the crate).
    pub format: u32,
    /// Active width, in pixels.
    pub act_w: u16,
    /// Active height, in pixels.
    pub act_h: u16,
    /// Horizontal offset of the active rectangle inside the virtual image.
    pub x_offset: u16,
    /// Vertical offset of the active rectangle inside the virtual image.
    pub y_offset: u16,
    /// Virtual (stride) width, in pixels.
    pub vir_w: u16,
    /// Virtual height, in pixels.
    pub vir_h: u16,
    /// `endian_mode` for the bit-depth formats (0 for 8-bit packed).
    pub endian_mode: u16,
    /// `alpha_swap`: unused by this crate (zero).
    pub alpha_swap: u16,
    /// RGA3 rotate mode of the image (zero: unused).
    pub rotate_mode: u16,
    /// RGA3 read mode (zero: raster).
    pub rd_mode: u16,
    /// RGA3 10-bit compact flag (zero).
    pub is_10b_compact: u16,
    /// RGA3 10-bit endian flag (zero).
    pub is_10b_endian: u16,
    /// Nonzero when the image is in use.
    pub enable: u16,
}

/// `POINT`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    /// X.
    pub x: u16,
    /// Y.
    pub y: u16,
}

/// `RECT`: an inclusive rectangle (`xmax` and `ymax` are the last pixel, width - 1).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    /// Left.
    pub xmin: u16,
    /// Right, inclusive.
    pub xmax: u16,
    /// Top.
    pub ymin: u16,
    /// Bottom, inclusive.
    pub ymax: u16,
}

/// `COLOR_FILL`: the gradient corners of a gradient fill (unused for solid fills).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorFill {
    /// A corner's X (`gr_x_a`).
    pub gr_x_a: i16,
    /// A corner's Y (`gr_y_a`).
    pub gr_y_a: i16,
    /// B corner's X (`gr_x_b`).
    pub gr_x_b: i16,
    /// B corner's Y (`gr_y_b`).
    pub gr_y_b: i16,
    /// G corner's X (`gr_x_g`).
    pub gr_x_g: i16,
    /// G corner's Y (`gr_y_g`).
    pub gr_y_g: i16,
    /// R corner's X (`gr_x_r`).
    pub gr_x_r: i16,
    /// R corner's Y (`gr_y_r`).
    pub gr_y_r: i16,
}

/// `FADING`: the fading colour (`b`, `g`, `r`, `res`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fading {
    /// Blue.
    pub b: u8,
    /// Green.
    pub g: u8,
    /// Red.
    pub r: u8,
    /// Reserved.
    pub res: u8,
}

/// `line_draw_t`: one line (unused: lines are not in this route).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineDraw {
    /// Start point.
    pub start_point: Point,
    /// End point.
    pub end_point: Point,
    /// Colour.
    pub color: u32,
    /// Drawing mode.
    pub flag: u32,
    /// Width, 1 to 16.
    pub line_width: u32,
}

/// `csc_coe_t`: one colour-space coefficient set.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CscCoe {
    /// R from V.
    pub r_v: i16,
    /// G from Y.
    pub g_y: i16,
    /// B from U.
    pub b_u: i16,
    /// Offset.
    pub off: i32,
}

/// `full_csc_t`: a full colour-space conversion (`flag` non-zero to use it).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FullCsc {
    /// Enables the conversion.
    pub flag: u8,
    /// Y coefficients.
    pub coe_y: CscCoe,
    /// U coefficients.
    pub coe_u: CscCoe,
    /// V coefficients.
    pub coe_v: CscCoe,
}

/// `MMU`: the address translation of the buffers. Set by [`Job`] only.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mmu {
    /// Enables the translation (1) for user addresses; 0 for dma-buf descriptors.
    pub mmu_en: u8,
    /// Base address of the mapping (zero).
    pub base_addr: u64,
    /// `[0]` MMU enable, `[1]` source flush, `[2]` destination flush, `[3]` command flush,
    /// `[4:5]` page size (from the header's comment).
    pub mmu_flag: u32,
}

/// `rga_mosaic_info`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MosaicInfo {
    /// Enables mosaic (zero).
    pub enable: u8,
    /// Mosaic mode (zero).
    pub mode: u8,
}

/// `rga_pre_intr_info`: pre-scaling interrupt settings (zero: none).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreIntrInfo {
    /// Enables.
    pub enable: u8,
    /// Read interrupt enable.
    pub read_intr_en: u8,
    /// Write interrupt enable.
    pub write_intr_en: u8,
    /// Read hold enable.
    pub read_hold_en: u8,
    /// Read threshold.
    pub read_threshold: u32,
    /// Write start.
    pub write_start: u32,
    /// Write step.
    pub write_step: u32,
}

/// `rga_osd_invert_factor`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OsdInvertFactor {
    /// Alpha maximum.
    pub alpha_max: u8,
    /// Alpha minimum.
    pub alpha_min: u8,
    /// Y/G maximum.
    pub yg_max: u8,
    /// Y/G minimum.
    pub yg_min: u8,
    /// C/RB maximum.
    pub crb_max: u8,
    /// C/RB minimum.
    pub crb_min: u8,
}

/// `rga_color`: a colour as its `u32` value; the bytes are red, green, blue, alpha in memory
/// order (`value` is little-endian on the targets).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Color {
    /// The packed colour.
    pub value: u32,
}

/// `rga_osd_bpp2`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OsdBpp2 {
    /// Alpha/colour swap (0: CA, 1: AC).
    pub ac_swap: u8,
    /// Endian swap (0: big, 1: little).
    pub endian_swap: u8,
    /// Colour 0.
    pub color0: Color,
    /// Colour 1.
    pub color1: Color,
}

/// `rga_osd_mode_ctrl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OsdModeCtrl {
    /// Calculation mode.
    pub mode: u8,
    /// Direction (0 horizontal, 1 vertical).
    pub direction_mode: u8,
    /// Width mode.
    pub width_mode: u8,
    /// Fixed block width.
    pub block_fix_width: u16,
    /// Block count.
    pub block_num: u8,
    /// Auto-invert flags index.
    pub flags_index: u16,
    /// Colour source.
    pub color_mode: u8,
    /// Invert flag source.
    pub invert_flags_mode: u8,
    /// Default colour.
    pub default_color_sel: u8,
    /// Channels enabled.
    pub invert_enable: u8,
    /// Invert calculation mode.
    pub invert_mode: u8,
    /// Luma threshold.
    pub invert_thresh: u8,
    /// Width configuration index.
    pub unfix_index: u8,
}

/// `rga_osd_info`. The two 64-bit flag words are the header's unions of two `u32`s.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OsdInfo {
    /// Enables OSD.
    pub enable: u8,
    /// Mode control.
    pub mode_ctrl: OsdModeCtrl,
    /// Invert factors.
    pub cal_factor: OsdInvertFactor,
    /// Two-bit-per-pixel colours.
    pub bpp2_info: OsdBpp2,
    /// The last flags (the header's `last_flags1:last_flags0` union, as one word).
    pub last_flags: u64,
    /// The current flags (the header's `cur_flags1:cur_flags0` union, as one word).
    pub cur_flags: u64,
}

/// `struct rga_req`: one synchronous request. Address fields are private: [`Job`] sets them from
/// the buffers it borrows. The rest are the header's fields, with the header's meanings (and the
/// unverified ones marked).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RgaReq {
    /// `render_mode`: 0 bit blit, 1 palette, 2 solid colour fill, 3 line drawing, 4 blur or
    /// sharpen, 5 pre-scaling, 6 palette table update, 7 pattern update.
    pub render_mode: u8,
    src: ImgInfo,
    dst: ImgInfo,
    pat: ImgInfo,
    rop_mask_addr: u64,
    lut_addr: u64,
    /// The destination clip, inclusive (defaults to the destination's virtual size).
    pub clip: Rect,
    /// Rotation sine, 16.16 fixed point. Unverified sign convention for turns.
    pub sina: i32,
    /// Rotation cosine, 16.16 fixed point.
    pub cosa: i32,
    /// `alpha_rop_flag`: bit 0 alpha ROP enable, 1 ROP enable, 2 fading, 3 Porter-Duff enable,
    /// 4 alpha calculation mode, 5 dither, 6 gradient fill mode, 7 anti-aliasing, 8 NN quantize,
    /// 9 real colour mode (the header's comments).
    pub alpha_rop_flag: u16,
    /// `scale_mode`: 0 nearest, 1 bilinear, 2 bicubic.
    pub scale_mode: u8,
    /// Colour key maximum (`R, G, B, X` packed as the header's `u32`; byte order unverified).
    pub color_key_max: u32,
    /// Colour key minimum (byte order unverified).
    pub color_key_min: u32,
    /// Foreground colour (solid fills, and the constant of blends). Byte order unverified: the
    /// crate packs it as `0xAABBGGRR`.
    pub fg_color: u32,
    /// Background colour.
    pub bg_color: u32,
    /// Gradient colour fill corners (unused).
    pub gr_color: ColorFill,
    /// Line drawing (unused).
    pub line_draw_info: LineDraw,
    /// Fading (unused).
    pub fading: Fading,
    /// Porter-Duff mode (value encoding unverified).
    pub pd_mode: u8,
    /// Global alpha (0 to 255).
    pub alpha_global_value: u8,
    /// `rop_code`: the ROP code (table unverified: the crate sends the ROP3 codes of
    /// `im2d_type.h`, which is a mapping this header does not state).
    pub rop_code: u16,
    /// Blur or sharpen selector (zero).
    pub bsfilter_flag: u8,
    /// Palette depth: 0 for 1 bpp, 1 for 2 bpp, 2 for 4 bpp, 3 for 8 bpp (from the header).
    pub palette_mode: u8,
    /// `yuv2rgb_mode`: 0 BT.601 limited, 1 BT.601 full (JPEG), 2 BT.709; with `0x100` and up
    /// the RGB-to-YUV and YUV-to-YUV conversions the header lists.
    pub yuv2rgb_mode: u8,
    /// Endian mode of the pixels (0 big, 1 little).
    pub endian_mode: u8,
    /// `rotate_mode`: 0 none, 1 rotate by `sina` and `cosa`, 2 x mirror, 3 y mirror (from the
    /// header). Which mirror is horizontal is unverified.
    pub rotate_mode: u8,
    /// 0 solid colour, 1 pattern colour.
    pub color_fill_mode: u8,
    mmu_info: Mmu,
    /// `alpha_rop_mode`: alpha mode (`[0:1]`), ROP mode (`[2:3]`), zero mode (`[4]`), destination
    /// alpha mode (`[5]`, RGA1).
    pub alpha_rop_mode: u8,
    /// Source transparency mode (zero).
    pub src_trans_mode: u8,
    /// Dither mode (zero).
    pub dither_mode: u8,
    /// Full colour-space conversion, used when `flag` is set.
    pub full_csc: FullCsc,
    /// Input fence descriptor, or -1 for none (the convention is unverified).
    pub in_fence_fd: i32,
    /// Core mask (zero: scheduler's choice).
    pub core: u8,
    /// Priority (zero).
    pub priority: u8,
    /// Output fence descriptor, or -1 (unverified convention).
    pub out_fence_fd: i32,
    /// Handle flag (zero).
    pub handle_flag: u8,
    /// Mosaic (zero).
    pub mosaic_info: MosaicInfo,
    /// Chroma down-sampling of the horizontal (zero).
    pub uvhds_mode: u8,
    /// Chroma down-sampling of the vertical (zero).
    pub uvvds_mode: u8,
    /// OSD (zero: disabled).
    pub osd_info: OsdInfo,
    /// Pre-scaling interrupts (zero: none).
    pub pre_intr_info: PreIntrInfo,
    reserved: [u8; 59],
}

impl Default for RgaReq {
    fn default() -> Self {
        Self {
            render_mode: 0,
            src: ImgInfo::default(),
            dst: ImgInfo::default(),
            pat: ImgInfo::default(),
            rop_mask_addr: 0,
            lut_addr: 0,
            clip: Rect::default(),
            sina: 0,
            cosa: 0,
            alpha_rop_flag: 0,
            scale_mode: 0,
            color_key_max: 0,
            color_key_min: 0,
            fg_color: 0,
            bg_color: 0,
            gr_color: ColorFill::default(),
            line_draw_info: LineDraw::default(),
            fading: Fading::default(),
            pd_mode: 0,
            alpha_global_value: 0,
            rop_code: 0,
            bsfilter_flag: 0,
            palette_mode: 0,
            yuv2rgb_mode: 0,
            endian_mode: 0,
            rotate_mode: 0,
            color_fill_mode: 0,
            mmu_info: Mmu::default(),
            alpha_rop_mode: 0,
            src_trans_mode: 0,
            dither_mode: 0,
            full_csc: FullCsc::default(),
            in_fence_fd: 0,
            core: 0,
            priority: 0,
            out_fence_fd: 0,
            handle_flag: 0,
            mosaic_info: MosaicInfo::default(),
            uvhds_mode: 0,
            uvvds_mode: 0,
            osd_info: OsdInfo::default(),
            pre_intr_info: PreIntrInfo::default(),
            reserved: [0; 59],
        }
    }
}

/// The bytes of a palette table: 256 entries of four bytes.
pub const LUT_LEN: usize = 256 * 4;

/// The bytes of a `vir_w` by `vir_h` image in the `RK_FORMAT_*` value `format` (`rga.h`), for
/// the packed formats, NV12 (`YCbCr_420_SP`, chroma included) and the 8-bit ones; `None` for
/// any other format, which a mapped buffer then cannot carry.
pub fn frame_len(format: u32, vir_w: u16, vir_h: u16) -> Option<usize> {
    let pixels = usize::from(vir_w) * usize::from(vir_h);
    match format >> 8 {
        0x0 | 0x1 | 0x3 => Some(pixels * 4),
        0x2 | 0x7 => Some(pixels * 3),
        0xa => Some(pixels + 2 * usize::from(vir_w.div_ceil(2)) * usize::from(vir_h.div_ceil(2))),
        0x13 | 0x15 => Some(pixels),
        _ => None,
    }
    .filter(|_| format & 0xff == 0)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// A buffer a [`Job`] reads or writes, borrowed for as long as the job lives.
pub enum Buffer<'a> {
    /// A dma-buf the caller holds: the kernel takes its descriptor.
    Dmabuf(BorrowedFd<'a>),
    /// Bytes read by the engine, at their virtual address.
    Read(&'a [u8]),
    /// Bytes written by the engine, at their virtual address.
    Write(&'a mut [u8]),
}

/// One request and the buffers it borrows. The buffers stay borrowed until the job is dropped,
/// so the addresses in `req` stay valid for [`blit_sync`].
pub struct Job<'a> {
    req: RgaReq,
    held: Vec<Buffer<'a>>,
}

impl Default for Job<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Job<'a> {
    /// An empty request: no operation, no buffers, no fences.
    pub fn new() -> Self {
        Self {
            req: RgaReq {
                in_fence_fd: -1,
                out_fence_fd: -1,
                ..RgaReq::default()
            },
            held: Vec::new(),
        }
    }

    /// The request's scalar fields (the addresses are set only by [`Job::set_src`],
    /// [`Job::set_dst`] and [`Job::set_lut`]).
    pub fn req(&mut self) -> &mut RgaReq {
        &mut self.req
    }

    /// The request as it will be sent.
    pub fn request(&self) -> &RgaReq {
        &self.req
    }

    /// Sets the source image and its buffer. Refuses a mapped buffer shorter than the image
    /// (see [`frame_len`]) and an active rectangle outside the virtual image.
    pub fn set_src(&mut self, info: ImgInfo, buffer: Buffer<'a>) -> io::Result<()> {
        self.req.src = Self::image(info, &buffer)?;
        self.held.push(buffer);
        Ok(())
    }

    /// Sets the destination image and its buffer, which the engine writes: a
    /// [`Buffer::Read`] is refused, as are the cases [`Job::set_src`] refuses.
    pub fn set_dst(&mut self, info: ImgInfo, buffer: Buffer<'a>) -> io::Result<()> {
        if matches!(buffer, Buffer::Read(_)) {
            return Err(invalid(
                "the destination is written: a read-only buffer is refused",
            ));
        }
        self.req.dst = Self::image(info, &buffer)?;
        self.held.push(buffer);
        Ok(())
    }

    /// Sets the palette table (256 entries of four bytes), read by the engine. Refuses a
    /// shorter table.
    pub fn set_lut(&mut self, table: &'a [u8]) -> io::Result<()> {
        if table.len() < LUT_LEN {
            return Err(invalid("a palette table is 256 entries of four bytes"));
        }
        self.req.lut_addr = table.as_ptr() as u64;
        Ok(())
    }

    /// The image with the address and translation of `buffer`. The MMU flags for a mapped
    /// buffer are the header's: enable, then the flush of the side it is on (the flush bits
    /// are unverified for this use).
    fn image(mut info: ImgInfo, buffer: &Buffer<'_>) -> io::Result<ImgInfo> {
        let fits = |offset: u16, active: u16, virtual_: u16| {
            u32::from(offset) + u32::from(active) <= u32::from(virtual_)
        };
        if !fits(info.x_offset, info.act_w, info.vir_w)
            || !fits(info.y_offset, info.act_h, info.vir_h)
        {
            return Err(invalid(
                "the active rectangle lies outside the virtual image",
            ));
        }
        let mapped = |len: usize| match frame_len(info.format, info.vir_w, info.vir_h) {
            Some(need) if len >= need => Ok(()),
            Some(_) => Err(invalid("the buffer is shorter than the image")),
            None => Err(invalid("a mapped buffer needs a format of known size")),
        };
        info.uv_addr = 0;
        info.v_addr = 0;
        info.yrgb_addr = match buffer {
            Buffer::Dmabuf(fd) => {
                use std::os::fd::AsRawFd;
                fd.as_raw_fd() as u64
            }
            Buffer::Read(bytes) => {
                mapped(bytes.len())?;
                bytes.as_ptr() as u64
            }
            Buffer::Write(bytes) => {
                mapped(bytes.len())?;
                bytes.as_ptr() as u64
            }
        };
        Ok(info)
    }
}

/// Runs `job` on the device at `fd` (`RGA_BLIT_SYNC`) and waits for it. The engine reads and
/// writes only the buffers the job borrows; the call returns when it is done.
pub fn blit_sync(fd: BorrowedFd<'_>, job: &mut Job<'_>) -> io::Result<()> {
    let req: *mut RgaReq = &mut job.req;
    // SAFETY: `RGA_BLIT_SYNC` takes a `struct rga_req`, which `job.req` is. Its addresses are
    // the buffers `job` borrows (only `Job` sets them), alive for this call, and the kernel
    // checks the dma-buf descriptors and the user mappings it is given.
    unsafe { ioctl_ptr(fd, BLIT_SYNC, req)? };
    Ok(())
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn request_number_matches_the_header() {
        assert_eq!(BLIT_SYNC, 0x5017);
    }

    #[test]
    fn image_info_matches_the_header() {
        assert_eq!(size_of::<ImgInfo>(), 56);
        assert_eq!(offset_of!(ImgInfo, format), 24);
        assert_eq!(offset_of!(ImgInfo, vir_w), 36);
        assert_eq!(offset_of!(ImgInfo, enable), 52);
        assert_eq!(size_of::<Point>(), 4);
        assert_eq!(size_of::<Rect>(), 8);
        assert_eq!(size_of::<Mmu>(), 24);
        assert_eq!(offset_of!(Mmu, base_addr), 8);
        assert_eq!(offset_of!(Mmu, mmu_flag), 16);
        assert_eq!(size_of::<ColorFill>(), 16);
        assert_eq!(size_of::<Fading>(), 4);
        assert_eq!(size_of::<LineDraw>(), 20);
        assert_eq!(offset_of!(LineDraw, line_width), 16);
        assert_eq!(size_of::<CscCoe>(), 12);
        assert_eq!(size_of::<FullCsc>(), 40);
        assert_eq!(offset_of!(FullCsc, coe_v), 28);
        assert_eq!(size_of::<MosaicInfo>(), 2);
        assert_eq!(size_of::<PreIntrInfo>(), 16);
        assert_eq!(offset_of!(PreIntrInfo, write_step), 12);
    }

    #[test]
    fn osd_matches_the_header() {
        assert_eq!(size_of::<OsdInvertFactor>(), 6);
        assert_eq!(size_of::<Color>(), 4);
        assert_eq!(size_of::<OsdBpp2>(), 12);
        assert_eq!(offset_of!(OsdBpp2, color1), 8);
        assert_eq!(size_of::<OsdModeCtrl>(), 18);
        assert_eq!(offset_of!(OsdModeCtrl, block_fix_width), 4);
        assert_eq!(offset_of!(OsdModeCtrl, flags_index), 8);
        assert_eq!(offset_of!(OsdModeCtrl, unfix_index), 16);
        assert_eq!(size_of::<OsdInfo>(), 56);
        assert_eq!(offset_of!(OsdInfo, mode_ctrl), 2);
        assert_eq!(offset_of!(OsdInfo, cal_factor), 20);
        assert_eq!(offset_of!(OsdInfo, bpp2_info), 28);
        assert_eq!(offset_of!(OsdInfo, last_flags), 40);
        assert_eq!(offset_of!(OsdInfo, cur_flags), 48);
    }

    #[test]
    fn request_matches_the_header() {
        assert_eq!(size_of::<RgaReq>(), 504);
        assert_eq!(offset_of!(RgaReq, src), 8);
        assert_eq!(offset_of!(RgaReq, dst), 64);
        assert_eq!(offset_of!(RgaReq, pat), 120);
        assert_eq!(offset_of!(RgaReq, rop_mask_addr), 176);
        assert_eq!(offset_of!(RgaReq, lut_addr), 184);
        assert_eq!(offset_of!(RgaReq, clip), 192);
        assert_eq!(offset_of!(RgaReq, sina), 200);
        assert_eq!(offset_of!(RgaReq, alpha_rop_flag), 208);
        assert_eq!(offset_of!(RgaReq, scale_mode), 210);
        assert_eq!(offset_of!(RgaReq, color_key_max), 212);
        assert_eq!(offset_of!(RgaReq, color_key_min), 216);
        assert_eq!(offset_of!(RgaReq, fg_color), 220);
        assert_eq!(offset_of!(RgaReq, bg_color), 224);
        assert_eq!(offset_of!(RgaReq, gr_color), 228);
        assert_eq!(offset_of!(RgaReq, line_draw_info), 244);
        assert_eq!(offset_of!(RgaReq, fading), 264);
        assert_eq!(offset_of!(RgaReq, pd_mode), 268);
        assert_eq!(offset_of!(RgaReq, alpha_global_value), 269);
        assert_eq!(offset_of!(RgaReq, rop_code), 270);
        assert_eq!(offset_of!(RgaReq, bsfilter_flag), 272);
        assert_eq!(offset_of!(RgaReq, palette_mode), 273);
        assert_eq!(offset_of!(RgaReq, yuv2rgb_mode), 274);
        assert_eq!(offset_of!(RgaReq, endian_mode), 275);
        assert_eq!(offset_of!(RgaReq, rotate_mode), 276);
        assert_eq!(offset_of!(RgaReq, color_fill_mode), 277);
        assert_eq!(offset_of!(RgaReq, mmu_info), 280);
        assert_eq!(offset_of!(RgaReq, alpha_rop_mode), 304);
        assert_eq!(offset_of!(RgaReq, dither_mode), 306);
        assert_eq!(offset_of!(RgaReq, full_csc), 308);
        assert_eq!(offset_of!(RgaReq, in_fence_fd), 348);
        assert_eq!(offset_of!(RgaReq, core), 352);
        assert_eq!(offset_of!(RgaReq, out_fence_fd), 356);
        assert_eq!(offset_of!(RgaReq, handle_flag), 360);
        assert_eq!(offset_of!(RgaReq, mosaic_info), 361);
        assert_eq!(offset_of!(RgaReq, uvhds_mode), 363);
        assert_eq!(offset_of!(RgaReq, osd_info), 368);
        assert_eq!(offset_of!(RgaReq, pre_intr_info), 424);
        assert_eq!(offset_of!(RgaReq, reserved), 440);
    }

    #[test]
    fn a_job_takes_its_addresses_from_its_buffers() {
        let pixels = [7u8; 8];
        let mut out = [0u8; 8];
        let mut job = Job::new();
        assert_eq!(job.request().in_fence_fd, -1);
        let two = ImgInfo {
            act_w: 2,
            act_h: 1,
            vir_w: 2,
            vir_h: 1,
            ..ImgInfo::default()
        };
        job.set_src(two, Buffer::Read(&pixels)).unwrap();
        job.set_dst(two, Buffer::Write(&mut out)).unwrap();
        assert_eq!(job.req.src.yrgb_addr, pixels.as_ptr() as u64);
        assert_eq!(job.req.dst.yrgb_addr, out.as_ptr() as u64);
    }

    #[test]
    fn a_job_refuses_buffers_the_engine_would_overrun() {
        let small = [0u8; 15];
        let mut out = [0u8; 16];
        let four = ImgInfo {
            act_w: 2,
            act_h: 2,
            vir_w: 2,
            vir_h: 2,
            ..ImgInfo::default()
        };
        let past = ImgInfo {
            x_offset: 1,
            ..four
        };
        let unknown = ImgInfo {
            format: 0x30 << 8,
            ..four
        };
        let mut job = Job::new();
        assert!(job.set_src(four, Buffer::Read(&small)).is_err());
        assert!(job.set_dst(four, Buffer::Read(&small)).is_err());
        assert!(job.set_lut(&small).is_err());
        assert!(Job::new().set_dst(past, Buffer::Write(&mut out)).is_err());
        assert!(
            Job::new()
                .set_dst(unknown, Buffer::Write(&mut out))
                .is_err()
        );
        assert!(Job::new().set_dst(four, Buffer::Write(&mut out)).is_ok());
        assert_eq!(frame_len(0xa << 8, 3, 3), Some(9 + 8));
    }
}
