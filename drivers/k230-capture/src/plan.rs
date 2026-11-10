//! What the driver writes: the output formats, each frame's planes, and the register sequences
//! that set the frame path up and take it down. Pure: nothing here touches the block.
//!
//! The sequences follow the order the ISP8000's open driver programs the same registers
//! (`set_data_path` in `vvcam/isp/isp_miv2.c`, `isp_s_input`, `isp_s_mux` and `isp_start_stream`
//! in `vvcam/isp/isp_ioctl.c`, `github.com/nxp-imx/isp-vvcam` at
//! `e6453233e67f5d2f180d6ec277158dd54fa67a25`): the ISP stopped, the input and windows, the main
//! path's format and geometry, then the memory interface armed and switched on (by the sys
//! crate), then the ISP enabled and its registers updated.
//!
//! Every write here passes the sys crate's table: no value here is an address.

use lemnos_contract_camera_receiver::{BayerOrder, Packing};
use lemnos_driver_k230_capture_sys::Output;
use lemnos_driver_k230_capture_sys::memory::{Plane, Planes};
use lemnos_driver_k230_capture_sys::regs::{
    ACQ_BAYER_PAT_SHIFT, ACQ_INPUT_SELECTION_SHIFT, BUS_CFG_SWAP_RAW_SHIFT, BUS_ID_SW_EN,
    BUS_ID_WR_ID_EN, CONV_INPUT_RGB888, CONV_OUTPUT_RGB888, CONV_OUTPUT_SHIFT, DEMOSAIC_BYPASS,
    FMT_NV12, FMT_RAW_ALIGNED_SHIFT, FMT_RAW_BIT_SHIFT, FMT_YUV_ALIGNED, FMT_YUV_BIT_10,
    FMT_YUV_FMT_SHIFT, FMT_YUV_STR_SHIFT, INPUT_8BIT, INPUT_10BIT, INPUT_12BIT, ISP_ACQ_H_OFFS,
    ISP_ACQ_H_SIZE, ISP_ACQ_NR_FRAMES, ISP_ACQ_PROP, ISP_ACQ_V_OFFS, ISP_ACQ_V_SIZE, ISP_CTRL,
    ISP_CTRL_CFG_UPD, ISP_CTRL_ENABLE, ISP_CTRL_GEN_CFG_UPD, ISP_CTRL_INFORM_ENABLE,
    ISP_CTRL_MODE_SHIFT, ISP_DEMOSAIC, ISP_IS_CTRL, ISP_IS_H_OFFS, ISP_IS_H_SIZE, ISP_IS_V_OFFS,
    ISP_IS_V_SIZE, ISP_MODE_RAW, ISP_MODE_RGB, ISP_OUT_H_OFFS, ISP_OUT_H_SIZE, ISP_OUT_V_OFFS,
    ISP_OUT_V_SIZE, MP_BUS_CFG, MP_BUS_ID, MP_FMT, MP_RAW_LLENGTH, MP_RAW_PIC_HEIGHT,
    MP_RAW_PIC_SIZE, MP_RAW_PIC_WIDTH, MP_Y_LLENGTH, MP_Y_PIC_HEIGHT, MP_Y_PIC_SIZE,
    MP_Y_PIC_WIDTH, MRSZ_CTRL, MRSZ_FORMAT_CONV_CTRL, RAW_ALIGNED_16, RAW_UNALIGNED,
    VI_CHAN_MODE_MP, VI_DPCL, VI_DPCL_CHAN_MODE_SHIFT, VI_DPCL_MP_MUX_MI,
};

/// Smallest frame in each direction.
pub const MIN_DIMENSION: u32 = 64;
/// Largest frame in each direction. The window registers take 14 bits; the vendor's own sensor
/// modes go to 3840 x 2160.
pub const MAX_DIMENSION: u32 = 4096;
/// Widths are multiples of this, so every line of every format is a whole number of the memory
/// interface's 16-byte units and its line lengths in pixels match the bytes.
pub const WIDTH_STEP: u32 = 16;

/// What the main path writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// The sensor's samples, as the link carries them: 8 bits in a byte, 10 or 12 bits in a
    /// 16-bit little-endian word, most significant bit aligned (the memory interface's 16-bit
    /// aligned mode; `ISP_PICBUF_TYPE_RAW10` in `isp_types.h`: "MSBit aligned, LSByte first").
    Bayer,
    /// YUV 4:2:0, 8 bits: the Y plane, then interleaved Cb and Cr at half height.
    Nv12,
    /// YUV 4:2:2, 8 bits: the Y plane, then interleaved Cb and Cr at full height.
    Nv16,
    /// YUV 4:2:2, 8 bits, interleaved Y, Cb, Y, Cr.
    Yuyv,
    /// 8-bit blue, green, red, interleaved.
    Bgr24,
    /// YUV 4:2:0, 10 bits in the top of 16-bit little-endian words: the Y plane, then interleaved
    /// Cb and Cr at half height.
    P010,
}

impl Format {
    /// Every format, in the order of [`Format::name`]s.
    pub const ALL: [Self; 6] = [
        Self::Bayer,
        Self::Nv12,
        Self::Nv16,
        Self::Yuyv,
        Self::Bgr24,
        Self::P010,
    ];

    /// The board file's spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bayer => "bayer",
            Self::Nv12 => "nv12",
            Self::Nv16 => "nv16",
            Self::Yuyv => "yuyv",
            Self::Bgr24 => "bgr24",
            Self::P010 => "p010",
        }
    }

    /// The format spelled `name`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.name() == name)
    }
}

/// One configured frame: how it lies in memory and the values the registers take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    /// Bits per sample in memory's terms: the link's for raw, 10 for P010, else 8.
    pub bit_depth: u8,
    pub packing: Packing,
    /// Bytes per line of the first plane.
    pub stride: u32,
    /// The planes, back to back from offset 0.
    pub planes: Planes,
    /// Bytes of one frame: every plane.
    pub bytes: u32,
    /// The path that writes it.
    pub output: Output,
    /// `miv2_mp_fmt`.
    pub fmt: u32,
}

/// The bytes per pixel of the first plane, and the second plane's height in lines (0: none).
const fn shape(format: Format, depth: u8, height: u32) -> (u32, u32) {
    match format {
        Format::Bayer if depth == 8 => (1, 0),
        Format::Bayer => (2, 0),
        Format::Nv12 => (1, height / 2),
        Format::Nv16 => (1, height),
        Format::Yuyv => (2, 0),
        Format::Bgr24 => (3, 0),
        Format::P010 => (2, height / 2),
    }
}

/// The layout of `format` at `width` x `height` from a link of `depth` bits, or `None` for a size
/// or depth it cannot take: widths that are not multiples of [`WIDTH_STEP`], odd heights, sizes
/// outside [`MIN_DIMENSION`]..=[`MAX_DIMENSION`], depths other than 8, 10 and 12.
#[must_use]
pub fn layout(format: Format, depth: u8, width: u32, height: u32) -> Option<Layout> {
    let dims = MIN_DIMENSION..=MAX_DIMENSION;
    if !dims.contains(&width)
        || !dims.contains(&height)
        || !width.is_multiple_of(WIDTH_STEP)
        || !height.is_multiple_of(2)
        || !matches!(depth, 8 | 10 | 12)
    {
        return None;
    }
    let (bytes_per_pixel, chroma_lines) = shape(format, depth, height);
    // At most 4096 * 3 * 4096 * 2: no overflow in u32.
    let stride = width * bytes_per_pixel;
    let y = Plane {
        offset: 0,
        size: stride * height,
    };
    let cbcr = (chroma_lines > 0).then_some(Plane {
        offset: y.size,
        size: stride * chroma_lines,
    });
    let planes = Planes::new(y, cbcr, None).ok()?;
    let bytes = u32::try_from(planes.end()).ok()?;
    let (bit_depth, packing) = match format {
        Format::Bayer if depth == 8 => (8, Packing::Packed),
        Format::Bayer => (depth, Packing::Unpacked16),
        Format::P010 => (10, Packing::Unpacked16),
        _ => (8, Packing::Packed),
    };
    let semi = FMT_NV12;
    let fmt = match format {
        Format::Bayer => {
            let (bit, aligned) = match depth {
                8 => (0, RAW_UNALIGNED),
                10 => (1, RAW_ALIGNED_16),
                _ => (2, RAW_ALIGNED_16),
            };
            (bit << FMT_RAW_BIT_SHIFT) | (aligned << FMT_RAW_ALIGNED_SHIFT)
        }
        Format::Nv12 => semi,
        Format::Nv16 => semi | (1 << FMT_YUV_FMT_SHIFT),
        Format::Yuyv => (1 << FMT_YUV_FMT_SHIFT) | (1 << FMT_YUV_STR_SHIFT),
        Format::Bgr24 => (2 << FMT_YUV_FMT_SHIFT) | (1 << FMT_YUV_STR_SHIFT),
        Format::P010 => semi | FMT_YUV_BIT_10 | FMT_YUV_ALIGNED,
    };
    let output = if format == Format::Bayer {
        Output::Raw
    } else {
        Output::YCbCr
    };
    Some(Layout {
        format,
        width,
        height,
        bit_depth,
        packing,
        stride,
        planes,
        bytes,
        output,
        fmt,
    })
}

/// `bayer_pat` for the colour of the first two pixels.
#[must_use]
pub const fn bayer_pattern(order: BayerOrder) -> Option<u32> {
    match order {
        BayerOrder::Rggb => Some(0),
        BayerOrder::Grbg => Some(1),
        BayerOrder::Gbrg => Some(2),
        BayerOrder::Bggr => Some(3),
        BayerOrder::Mono => None,
    }
}

/// Register writes for one step: at most [`MAX_WRITES`].
pub const MAX_WRITES: usize = 32;

/// A fixed list of register writes, made without allocating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Writes {
    pairs: [(u32, u32); MAX_WRITES],
    len: usize,
}

impl Writes {
    const fn new() -> Self {
        Self {
            pairs: [(0, 0); MAX_WRITES],
            len: 0,
        }
    }

    fn push(&mut self, addr: u32, value: u32) {
        self.pairs[self.len] = (addr, value);
        self.len += 1;
    }

    /// The writes, in order.
    #[must_use]
    pub fn as_slice(&self) -> &[(u32, u32)] {
        &self.pairs[..self.len]
    }
}

/// The ISP stopped: disabled and its input formatter off, taking effect at the next frame.
#[must_use]
pub fn halt() -> Writes {
    let mut w = Writes::new();
    w.push(ISP_CTRL, ISP_CTRL_GEN_CFG_UPD);
    w
}

/// The input, windows and main-path format for `layout` from a sensor of `order` at `depth` bits,
/// written while the ISP is stopped and before the memory interface is armed.
#[must_use]
pub fn setup(layout: &Layout, pattern: u32, depth: u8) -> Writes {
    let (w, h) = (layout.width, layout.height);
    let raw = layout.output == Output::Raw;
    let input = match depth {
        8 => INPUT_8BIT,
        10 => INPUT_10BIT,
        _ => INPUT_12BIT,
    };
    let mut out = Writes::new();
    out.push(
        VI_DPCL,
        VI_DPCL_MP_MUX_MI | (VI_CHAN_MODE_MP << VI_DPCL_CHAN_MODE_SHIFT),
    );
    // The latency FIFO left at 0: the input formatter, not the DMA read port.
    out.push(
        ISP_ACQ_PROP,
        (pattern << ACQ_BAYER_PAT_SHIFT) | (input << ACQ_INPUT_SELECTION_SHIFT),
    );
    for (addr, value) in [
        (ISP_ACQ_H_OFFS, 0),
        (ISP_ACQ_V_OFFS, 0),
        (ISP_ACQ_H_SIZE, w),
        (ISP_ACQ_V_SIZE, h),
        // No end: frames until stopped.
        (ISP_ACQ_NR_FRAMES, 0),
        (ISP_OUT_H_OFFS, 0),
        (ISP_OUT_V_OFFS, 0),
        (ISP_OUT_H_SIZE, w),
        (ISP_OUT_V_SIZE, h),
        (ISP_IS_CTRL, 0),
        (ISP_IS_H_OFFS, 0),
        (ISP_IS_V_OFFS, 0),
        (ISP_IS_H_SIZE, w),
        (ISP_IS_V_SIZE, h),
        (ISP_DEMOSAIC, if raw { DEMOSAIC_BYPASS } else { 0 }),
        // No scaling.
        (MRSZ_CTRL, 0),
    ] {
        out.push(addr, value);
    }
    let conv = if layout.format == Format::Bgr24 {
        (CONV_OUTPUT_RGB888 << CONV_OUTPUT_SHIFT) | CONV_INPUT_RGB888
    } else {
        0
    };
    out.push(MRSZ_FORMAT_CONV_CTRL, conv);
    out.push(MP_FMT, layout.fmt);
    let swap = u32::from(raw && depth > 8);
    out.push(MP_BUS_CFG, swap << BUS_CFG_SWAP_RAW_SHIFT);
    out.push(MP_BUS_ID, BUS_ID_WR_ID_EN | BUS_ID_SW_EN);
    if raw {
        out.push(MP_RAW_LLENGTH, layout.stride);
        out.push(MP_RAW_PIC_WIDTH, w);
        out.push(MP_RAW_PIC_HEIGHT, h);
        out.push(MP_RAW_PIC_SIZE, layout.stride * h);
    } else {
        // In pixels, as `set_data_path` writes them.
        out.push(MP_Y_LLENGTH, w);
        out.push(MP_Y_PIC_WIDTH, w);
        out.push(MP_Y_PIC_HEIGHT, h);
        out.push(MP_Y_PIC_SIZE, w * h);
    }
    out
}

/// The ISP enabled in the mode `layout` needs, then its registers updated now and at the next
/// frame. Written after the memory interface is armed and on.
#[must_use]
pub fn run(layout: &Layout) -> Writes {
    let mode = if layout.output == Output::Raw {
        ISP_MODE_RAW
    } else {
        ISP_MODE_RGB
    };
    let on = ISP_CTRL_ENABLE | ISP_CTRL_INFORM_ENABLE | (mode << ISP_CTRL_MODE_SHIFT);
    let mut w = Writes::new();
    w.push(ISP_CTRL, on);
    w.push(ISP_CTRL, on | ISP_CTRL_CFG_UPD | ISP_CTRL_GEN_CFG_UPD);
    w
}
