//! The vendor kernel's multi-RGA request, `/dev/rga`: `RGA_BLIT_SYNC` and `struct rga_req`, as
//! the vendor kernel's RGA2 and RGA3 drivers take it.
//!
//! Copyright (C) 2016 Rockchip Electronics Co., Ltd. (authors: Zhiqin Wei, Yu Qiaowei), licensed
//! under the Apache License, Version 2.0. The structures are transcribed from `include/rga/rga.h`
//! of the librga release at commit 331d5750 (`release_version.txt`, version 1.8.1), and the
//! request number is `RGA_BLIT_SYNC` 0x5017 there and in `drmrga.h`. The C compiler's sizes and
//! offsets on LP64 are pinned by the tests below. No GPL kernel header was used.
//!
//! # What a request can make the engine touch
//!
//! The request holds no pointers, only 64-bit numbers the kernel follows. An image channel whose
//! `yrgb_addr` is non-zero names a dma-buf by its descriptor; one whose `yrgb_addr` is zero is
//! read from `uv_addr`, as a user address or, with the MMU off, as a physical address. A safe
//! caller must never be able to reach the second form, so:
//!
//! - Images are dma-bufs only ([`Job::set_src`], [`Job::set_dst`]), and the descriptor is a
//!   [`BorrowedFd`] the job keeps borrowed until it is dropped, so it names memory the caller
//!   holds for the whole call. Descriptor 0 is refused: it would read as "no dma-buf".
//! - Each image's extent (its virtual width and height in its format, see [`frame_len`]) is
//!   checked against the dma-buf's real size, asked of the kernel, with overflow-free
//!   arithmetic; its active rectangle must lie inside its virtual image.
//! - Every other address field (`uv_addr`, `v_addr`, the pattern image, the ROP mask and the
//!   palette table) stays zero, and nothing here sets the MMU, the buffer handles, OSD, mosaic or
//!   the pre-scaling interrupts. Only a blit and a solid fill can be sent ([`Mode`]); a blit
//!   needs its source set. The destination clip must lie inside the destination image.
//!
//! Byte buffers at their virtual address are not offered: the dev branch's mapping of them
//! (a user address in `yrgb_addr`) reads as a dma-buf descriptor to the vendor driver, and the
//! form that would carry one (`uv_addr` with the MMU flags) is unverified. Nor is the palette
//! table, whose address field's form is unverified.
//!
//! Field mappings the header does not spell out are marked in each item's docs as unverified.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

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

/// `MMU`: the address translation of the buffers. Never set by this crate: always zero.
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

impl RgaReq {
    /// The source image as it will be sent.
    pub fn src(&self) -> &ImgInfo {
        &self.src
    }

    /// The destination image as it will be sent.
    pub fn dst(&self) -> &ImgInfo {
        &self.dst
    }
}

/// The bytes of a `vir_w` by `vir_h` image in the `RK_FORMAT_*` value `format` (`rga.h`), for
/// the 32-bit (`RGBA_8888`, `RGBX_8888`, `BGRA_8888`) and 24-bit (`RGB_888`, `BGR_888`) packed
/// formats, `YCbCr_420_SP` (chroma included) and the 8-bit ones (`BPP8`, `YCbCr_400`); `None`
/// for any other format, which a request then refuses.
pub fn frame_len(format: u32, vir_w: u16, vir_h: u16) -> Option<u64> {
    if format & 0xff != 0 {
        return None;
    }
    let (w, h) = (u64::from(vir_w), u64::from(vir_h));
    let pixels = w * h;
    match format >> 8 {
        0x0 | 0x1 | 0x3 => Some(pixels * 4),
        0x2 | 0x7 => Some(pixels * 3),
        0xa => Some(pixels + 2 * w.div_ceil(2) * h.div_ceil(2)),
        0x13 | 0x15 => Some(pixels),
        _ => None,
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// What a request does (`render_mode`). Only these two are sent: the other modes read the
/// pattern image or the palette table, which this crate never sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `bitblt` (0): source to destination, with the request's conversion, ROP, blend, colour
    /// key or transform.
    Blit,
    /// `color_fill` (2): a solid colour onto the destination's active rectangle.
    Fill,
}

/// A ROP code of `im2d_type.h` (Apache-2.0). Only these three are sent, as none of them names
/// the pattern image. The request's ROP table is not stated in the header: field mapping
/// unverified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum RopCode {
    /// `IM_ROP_AND`.
    And = 0x88,
    /// `IM_ROP_OR`.
    Or = 0xee,
    /// `IM_ROP_XOR`.
    Xor = 0xf6,
}

/// A Porter-Duff mode's `pd_mode` value. Field mapping unverified: the header names the field
/// "porter duff alpha mode sel" but not its values; these are an assumption a board run checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PorterDuff {
    /// The foreground replaces the destination.
    Src = 0,
    /// The foreground over the destination.
    SrcOver = 1,
    /// The destination over the foreground.
    DstOver = 2,
}

/// A mirror or quarter turn: `rotate_mode` (`rga.h`: 1 rotate by `sina` and `cosa`, 2 x mirror,
/// 3 y mirror) with the sine and cosine in 16.16. Which mirror is horizontal and which way a turn
/// goes are unverified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    /// `rotate_mode` 2.
    MirrorX,
    /// `rotate_mode` 3.
    MirrorY,
    /// A quarter turn, `sina` 1.
    Rotate90,
    /// Half a turn, `cosa` -1.
    Rotate180,
    /// Three quarters, `sina` -1.
    Rotate270,
}

/// `yuv2rgb_mode` for the conversions a blit makes (`rga.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum YuvMode {
    /// 0: BT.601 limited range.
    Bt601Limited = 0,
    /// 1: BT.601 full range ("JPEG").
    Bt601Full = 1,
    /// 2: BT.709 limited range.
    Bt709Limited = 2,
}

/// `alpha_rop_flag` bits (`rga.h`).
const ALPHA_ROP_ENABLE: u16 = 0x1;
const ROP_ENABLE: u16 = 0x2;
const PD_ENABLE: u16 = 0x8;
/// `render_mode` values (`rga.h`).
const BITBLT: u8 = 0x0;
const COLOR_FILL: u8 = 0x2;
/// 1.0 in the header's 16.16 sine and cosine.
const ONE: i32 = 1 << 16;

/// One request and the dma-bufs it names. The descriptors stay borrowed until the job is
/// dropped, so the numbers in the request name memory the caller holds for [`blit_sync`].
#[derive(Debug)]
pub struct Job<'a> {
    req: RgaReq,
    src: Option<BorrowedFd<'a>>,
    dst: Option<BorrowedFd<'a>>,
}

impl<'a> Job<'a> {
    /// An empty request of `mode`: no images, no fences.
    pub fn new(mode: Mode) -> Self {
        Self {
            req: RgaReq {
                render_mode: match mode {
                    Mode::Blit => BITBLT,
                    Mode::Fill => COLOR_FILL,
                },
                in_fence_fd: -1,
                out_fence_fd: -1,
                ..RgaReq::default()
            },
            src: None,
            dst: None,
        }
    }

    /// The request as it will be sent.
    pub fn request(&self) -> &RgaReq {
        &self.req
    }

    /// Sets the source image, read from the dma-buf `buffer`. Refuses what
    /// [`Job::set_dst`] refuses.
    pub fn set_src(&mut self, info: ImgInfo, buffer: BorrowedFd<'a>) -> io::Result<()> {
        self.req.src = image(info, buffer)?;
        self.src = Some(buffer);
        Ok(())
    }

    /// Sets the destination image, written to the dma-buf `buffer`, and the clip to the whole of
    /// it. Refuses descriptor 0, a format of unknown size, an active rectangle outside the
    /// virtual image, and a dma-buf smaller than the virtual image.
    pub fn set_dst(&mut self, info: ImgInfo, buffer: BorrowedFd<'a>) -> io::Result<()> {
        self.req.dst = image(info, buffer)?;
        self.req.clip = Rect {
            xmin: 0,
            xmax: info.vir_w.saturating_sub(1),
            ymin: 0,
            ymax: info.vir_h.saturating_sub(1),
        };
        self.dst = Some(buffer);
        Ok(())
    }

    /// The destination clip, inclusive. [`blit_sync`] refuses one outside the destination.
    pub fn set_clip(&mut self, clip: Rect) {
        self.req.clip = clip;
    }

    /// A ROP of the source onto the destination.
    pub fn set_rop(&mut self, code: RopCode) {
        self.req.alpha_rop_flag = ROP_ENABLE;
        self.req.rop_code = code as u16;
    }

    /// A Porter-Duff blend of the source onto the destination, at full global alpha.
    /// `premultiplied` goes in `alpha_rop_mode` as 1 (encoding unverified).
    pub fn set_blend(&mut self, mode: PorterDuff, premultiplied: bool) {
        self.req.alpha_rop_flag = ALPHA_ROP_ENABLE | PD_ENABLE;
        self.req.pd_mode = mode as u8;
        self.req.alpha_rop_mode = u8::from(premultiplied);
        self.req.alpha_global_value = 0xff;
    }

    /// A fill's solid colour (`fg_color`; byte order unverified).
    pub fn set_fill_color(&mut self, color: u32) {
        self.req.color_fill_mode = 0;
        self.req.fg_color = color;
    }

    /// The colour key range (`color_key_min`, `color_key_max`; byte order unverified).
    pub fn set_color_key(&mut self, min: u32, max: u32) {
        self.req.color_key_min = min;
        self.req.color_key_max = max;
    }

    /// A mirror or quarter turn.
    pub fn set_turn(&mut self, turn: Turn) {
        let (mode, sina, cosa) = match turn {
            Turn::MirrorX => (2, 0, 0),
            Turn::MirrorY => (3, 0, 0),
            Turn::Rotate90 => (1, ONE, 0),
            Turn::Rotate180 => (1, 0, -ONE),
            Turn::Rotate270 => (1, -ONE, 0),
        };
        self.req.rotate_mode = mode;
        self.req.sina = sina;
        self.req.cosa = cosa;
    }

    /// The matrix of a YUV and RGB conversion.
    pub fn set_yuv_mode(&mut self, mode: YuvMode) {
        self.req.yuv2rgb_mode = mode as u8;
    }

    /// The user colour-space conversion (`full_csc`, RGA2 cores).
    pub fn set_full_csc(&mut self, csc: FullCsc) {
        self.req.full_csc = csc;
    }

    /// The checks [`blit_sync`] makes before the request leaves: a destination, a source for a
    /// blit, and a clip inside the destination.
    pub fn check(&self) -> io::Result<()> {
        if self.dst.is_none() {
            return Err(invalid("a request needs its destination"));
        }
        if self.req.render_mode == BITBLT && self.src.is_none() {
            return Err(invalid("a blit needs its source"));
        }
        let (clip, dst) = (self.req.clip, self.req.dst);
        if clip.xmin > clip.xmax
            || clip.ymin > clip.ymax
            || clip.xmax >= dst.vir_w
            || clip.ymax >= dst.vir_h
        {
            return Err(invalid("the clip lies outside the destination"));
        }
        Ok(())
    }
}

/// `info` with the descriptor of `buffer` as its address, after the checks of [`Job::set_dst`].
fn image(mut info: ImgInfo, buffer: BorrowedFd<'_>) -> io::Result<ImgInfo> {
    let fits = |offset: u16, active: u16, virtual_: u16| {
        u32::from(offset) + u32::from(active) <= u32::from(virtual_)
    };
    if info.act_w == 0
        || info.act_h == 0
        || !fits(info.x_offset, info.act_w, info.vir_w)
        || !fits(info.y_offset, info.act_h, info.vir_h)
    {
        return Err(invalid(
            "the active rectangle is empty or lies outside the virtual image",
        ));
    }
    let need = frame_len(info.format, info.vir_w, info.vir_h)
        .ok_or_else(|| invalid("an image needs a format of known size"))?;
    let raw = buffer.as_raw_fd();
    let address = u64::try_from(raw)
        .ok()
        .filter(|&a| a != 0)
        .ok_or_else(|| invalid("descriptor 0 reads as no dma-buf in a request"))?;
    if lemnos_linux_sys::dma_buf::size(buffer)? < need {
        return Err(invalid("the dma-buf is smaller than the image"));
    }
    info.yrgb_addr = address;
    info.uv_addr = 0;
    info.v_addr = 0;
    Ok(info)
}

/// Runs `job` on the device at `fd` (`RGA_BLIT_SYNC`) and waits for it, after [`Job::check`].
/// The engine reads and writes only the dma-bufs the job borrows, each checked to hold its image.
pub fn blit_sync(fd: BorrowedFd<'_>, job: &mut Job<'_>) -> io::Result<()> {
    job.check()?;
    let req: *mut RgaReq = &mut job.req;
    loop {
        // SAFETY: `RGA_BLIT_SYNC` takes a `struct rga_req`, which `job.req` is, live and
        // exclusively borrowed for the call. Its only non-zero addresses are the descriptors of
        // the dma-bufs `job` borrows, each checked to hold its image (see the module docs).
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), BLIT_SYNC as _, req) };
        if ret >= 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use std::fs::File;
    use std::mem::{offset_of, size_of};
    use std::os::fd::AsFd;

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

    /// A memfd of `len` bytes: it answers the size query as a dma-buf does.
    fn buffer(len: u64) -> File {
        let file = File::from(lemnos_linux_sys::dma_buf::memfd(c"rga-test").unwrap());
        file.set_len(len).unwrap();
        file
    }

    fn rgba(w: u16, h: u16) -> ImgInfo {
        ImgInfo {
            act_w: w,
            act_h: h,
            vir_w: w,
            vir_h: h,
            enable: 1,
            ..ImgInfo::default()
        }
    }

    #[test]
    fn a_job_names_its_dma_bufs_and_nothing_else() {
        let (src, dst) = (buffer(32), buffer(32));
        let mut job = Job::new(Mode::Blit);
        assert_eq!(job.request().in_fence_fd, -1);
        job.set_src(rgba(4, 2), src.as_fd()).unwrap();
        job.set_dst(rgba(4, 2), dst.as_fd()).unwrap();
        let req = job.request();
        assert_eq!(req.src.yrgb_addr, src.as_raw_fd() as u64);
        assert_eq!(req.dst.yrgb_addr, dst.as_raw_fd() as u64);
        for image in [req.src, req.dst, req.pat] {
            assert_eq!((image.uv_addr, image.v_addr), (0, 0));
        }
        assert_eq!((req.rop_mask_addr, req.lut_addr), (0, 0));
        assert_eq!(req.mmu_info, Mmu::default());
        assert_eq!((req.clip.xmax, req.clip.ymax), (3, 1));
        job.check().unwrap();
    }

    #[test]
    fn a_dma_buf_smaller_than_its_image_is_refused() {
        // 4x2 RGBA is 32 bytes: one byte short is refused on either side.
        let short = buffer(31);
        let enough = buffer(32);
        let mut job = Job::new(Mode::Blit);
        assert!(job.set_src(rgba(4, 2), short.as_fd()).is_err());
        assert!(job.set_dst(rgba(4, 2), short.as_fd()).is_err());
        job.set_dst(rgba(4, 2), enough.as_fd()).unwrap();
        // The virtual image, not the active rectangle, is what must fit.
        let wide = ImgInfo {
            act_w: 1,
            vir_w: 5,
            ..rgba(4, 2)
        };
        assert!(job.set_src(wide, enough.as_fd()).is_err());
        // The largest image the fields can describe is checked without overflow.
        let huge = rgba(u16::MAX, u16::MAX);
        assert!(job.set_src(huge, enough.as_fd()).is_err());
    }

    #[test]
    fn rectangles_formats_and_clips_outside_the_image_are_refused() {
        let file = buffer(64);
        let past = ImgInfo {
            x_offset: 1,
            ..rgba(4, 2)
        };
        let empty = ImgInfo {
            act_w: 0,
            ..rgba(4, 2)
        };
        let unknown = ImgInfo {
            format: 0x30 << 8,
            ..rgba(4, 2)
        };
        let odd = ImgInfo {
            format: 0x1,
            ..rgba(4, 2)
        };
        for info in [past, empty, unknown, odd] {
            assert!(Job::new(Mode::Fill).set_dst(info, file.as_fd()).is_err());
        }
        let mut job = Job::new(Mode::Fill);
        job.set_dst(rgba(4, 2), file.as_fd()).unwrap();
        job.check().unwrap();
        job.set_clip(Rect {
            xmin: 0,
            xmax: 4,
            ymin: 0,
            ymax: 1,
        });
        assert!(job.check().is_err());
        assert_eq!(frame_len(0xa << 8, 3, 3), Some(9 + 8));
    }

    #[test]
    fn a_job_without_its_images_is_not_sent() {
        let file = buffer(32);
        assert!(Job::new(Mode::Fill).check().is_err());
        let mut job = Job::new(Mode::Blit);
        job.set_dst(rgba(4, 2), file.as_fd()).unwrap();
        assert!(job.check().is_err());
        // Refused before the device is reached: the descriptor is no RGA node.
        assert!(blit_sync(file.as_fd(), &mut job).is_err());
    }

    #[test]
    fn the_setters_write_the_header_fields() {
        let mut job = Job::new(Mode::Blit);
        job.set_rop(RopCode::Xor);
        assert_eq!(
            (job.request().alpha_rop_flag, job.request().rop_code),
            (2, 0xf6)
        );
        job.set_blend(PorterDuff::SrcOver, true);
        let req = job.request();
        assert_eq!(req.alpha_rop_flag, ALPHA_ROP_ENABLE | PD_ENABLE);
        assert_eq!(
            (req.pd_mode, req.alpha_rop_mode, req.alpha_global_value),
            (1, 1, 255)
        );
        job.set_turn(Turn::Rotate90);
        let req = job.request();
        assert_eq!((req.rotate_mode, req.sina, req.cosa), (1, ONE, 0));
        job.set_turn(Turn::MirrorY);
        assert_eq!(job.request().rotate_mode, 3);
        assert_eq!(Job::new(Mode::Fill).request().render_mode, COLOR_FILL);
    }
}
