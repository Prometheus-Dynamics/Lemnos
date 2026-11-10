//! The command stream: the words a [`crate::Context`] copies into a command buffer for the GPU.
//!
//! A stream is built only through the typed methods below, each of which checks what it writes,
//! so that the GPU reads and writes nothing but the buffers the stream borrows. There is no way to
//! add a raw word or write a register by number.
//!
//! # Every word a caller influences, and its check
//!
//! - **Addresses** (0x0A11 target, 0x0A29 source) come only from a [`Buffer`] borrowed for the
//!   stream's life. The trait is sealed: the kernel's [`crate::Memory`], and [`Scratch`], which
//!   [`crate::Context::run`] refuses. A buffer's address is its first byte; there is no offset.
//! - **Strides and sizes** (0x0A12, 0x0A13, 0x0A2B, 0x0A2F) come from a [`Layout`] checked against
//!   that buffer by [`Layout::extent`]: a row of pixels fits the stride, `stride * height` fits
//!   the buffer and the 32-bit GPU address space, the width and height fit their 16-bit fields,
//!   and the stride stays below bit 28, the tiling flag of the stride registers.
//! - **Formats** (0x0A10, 0x0A25) come from [`TargetFormat`] and [`SourceFormat`], which also set
//!   the bytes per pixel the layout is checked with, so the GPU's idea of a row and the check's
//!   agree. The other bits of 0x0A25 come from [`SourceOptions`]; the compression, YUV, tiling
//!   and index-endian bits cannot be set.
//! - **The configuration word** (0x0A00) is checked against the bits the encoders use
//!   ([`CONFIG_BITS`]): tiling, masks, scissor, colour transform and the other modes that make
//!   the GPU read memory of its own cannot be set. Its primitive mode decides what may follow: a
//!   rectangle only in a clear or image mode, a pixel-engine clear only in its mode, path data
//!   only in the path mode, and an image rectangle only once a source is set in this stream (so
//!   the GPU never samples a source address left over from an earlier stream).
//! - **Rectangles, clears and the tessellation window** must lie inside the render target set in
//!   this stream, and a pixel-engine clear may not exceed the target's extent.
//! - **Inline path data** is whole words inside the command buffer; it carries no address. It
//!   needs the path mode, a tessellation window and a path control word (0x0A34), which is
//!   checked against [`PATH_CONTROL_BITS`] (no tiling, no fill rule; FP32 data).
//! - **The tessellation buffer's** addresses and size come from the context, and
//!   [`crate::Context::run`] refuses a stream whose tessellation is not its own.
//! - **Plain values** with no address in them: the colour (0x0A02), the blit steps
//!   (0x0A18 to 0x0A22), the path scale, bias and matrix (0x0A3B, 0x0A3C, 0x0A40 to 0x0A45,
//!   0x0ACD, 0x0ACE), as floats. That the GPU's sampling stays inside the source's size and its
//!   drawing inside the target's size and the window, whatever those values, is the vendor's
//!   design and unverified on board.
//! - **Zero writes**: only to 0x0A27, 0x0A2D, 0x0A34, 0x0A39 and 0x0AD1, the registers the
//!   vendor writes as zero.
//! - **Capacity**: every method refuses to grow the stream past one command buffer
//!   (`OutOfMemory`), and adds nothing when it refuses.

use std::io;
use std::marker::PhantomData;

use crate::Tessellation;

/// The opcode of a state write, `VG_LITE_STATE(address)`.
const STATE: u32 = 0x3001_0000;
/// `VG_LITE_SEMAPHORE(7)` and `VG_LITE_STALL(7)`: the pixel engine's flush.
const SEMAPHORE_7: u32 = 0x1000_0007;
const STALL_7: u32 = 0x2000_0007;
/// `VG_LITE_DATA(n)`: the header of `n` eight-byte units of inline data (`push_data`).
const DATA: u32 = 0x4000_0000;
/// `VG_LITE_STATES(256, 0x0B00)`: the colour table's run header.
const CLUT_HEADER: u32 = 0x3100_0B00;
/// `VG_LITE_NOP()`: pads the colour table to an even count.
const NOP: u32 = 0x8000_0000;

/// The bits of the configuration word (0x0A00) a stream may set: `0x10000000` (set by the clear
/// and the path draw, and as `in_premult` by a blit of anything but A8), the primitive modes
/// (rectangle 0x1, pixel-engine clear 0x4, image 0x1000), the blends (SRC_OVER 0x100, DST_OVER
/// 0x200) and the rotated blit's transparency (0x8000), as `vg_lite.c` builds them.
pub const CONFIG_BITS: u32 = 0x1000_0000 | 0x8000 | 0x1000 | 0x0300 | 0x4 | 0x1;
/// The bits of the path control word (0x0A34) a stream may set beyond the required control bit
/// and FP32 format: the stroke bit (0x200) and the quality (`convert_path_quality`, the low two
/// bits).
pub const PATH_CONTROL_BITS: u32 = 0x200 | 0x3;
/// The path control word's required bits: the control bit (0x01000000) and FP32 path data
/// (`convert_path_format(VG_LITE_FP32)`, 0x300000).
const PATH_CONTROL_BASE: u32 = 0x0100_0000 | 0x0030_0000;
/// The stride registers' tiling flag (`tiled` in `set_render_target` and `vg_lite_blit`).
const STRIDE_LIMIT: u32 = 0x1000_0000;
/// Registers the vendor writes as zero, and a stream writes only as zero.
const ZERO_REGISTERS: [u32; 5] = [0x0A27, 0x0A2D, 0x0A34, 0x0A39, 0x0AD1];
/// The bits of the source word (0x0A25) [`SourceOptions`] sets, beyond the format code.
pub const SOURCE_REGISTER_BITS: u32 =
    SOURCE_BILINEAR | LUMA_CONVERSION | READ_SPLIT | SRC_PREMULTIPLY;
const SOURCE_BILINEAR: u32 = 0x2_0000;
const LUMA_CONVERSION: u32 = 0x8000_0000;
const READ_SPLIT: u32 = 0x80;
/// `src_premultiply_enable` with `HW_PREMULTIPLY` 0: always set, as the K230 build sets it.
const SRC_PREMULTIPLY: u32 = 0x0100_0000;
/// `premultiply_dst`: always set in the target word.
const PREMULTIPLY_DST: u32 = 0x100;

/// Something a [`Stream`] may name as a GPU buffer. Sealed: the kernel's [`crate::Memory`] and
/// the host-only [`Scratch`] are the only implementations.
pub trait Buffer: sealed::Sealed {
    /// The GPU address of the buffer's first byte.
    fn gpu_address(&self) -> u32;
    /// The bytes the buffer spans.
    fn byte_len(&self) -> usize;
    /// Whether the kernel owns the memory. [`crate::Context::run`] refuses a stream that names a
    /// buffer the kernel does not.
    fn kernel_backed(&self) -> bool;
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for crate::Memory {}
    impl Sealed for super::Scratch {}
}

impl Buffer for crate::Memory {
    fn gpu_address(&self) -> u32 {
        self.gpu
    }
    fn byte_len(&self) -> usize {
        self.len
    }
    fn kernel_backed(&self) -> bool {
        true
    }
}

/// An address and a length with no kernel memory behind them: for building and checking streams
/// on a host without a device. [`crate::Context::run`] refuses a stream that names one, so a
/// `Scratch` never reaches the GPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scratch {
    gpu: u32,
    bytes: usize,
}

impl Scratch {
    /// A stand-in buffer at `gpu_address` spanning `bytes` bytes. Not kernel memory.
    pub fn new(gpu_address: u32, bytes: usize) -> Self {
        Self {
            gpu: gpu_address,
            bytes,
        }
    }
}

impl Buffer for Scratch {
    fn gpu_address(&self) -> u32 {
        self.gpu
    }
    fn byte_len(&self) -> usize {
        self.bytes
    }
    fn kernel_backed(&self) -> bool {
        false
    }
}

/// A render target's format (`convert_target_format` of the K230's build).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TargetFormat {
    /// One alpha byte.
    A8,
    /// One luma byte.
    L8,
    /// R, G, B, A bytes.
    Rgba8888,
    /// B, G, R, A bytes.
    Bgra8888,
    /// R, G, B, X bytes.
    Rgbx8888,
}

impl TargetFormat {
    /// The format code of the target word.
    pub const fn code(self) -> u32 {
        match self {
            Self::A8 => 0x0,
            Self::L8 => 0x6,
            Self::Rgba8888 => 0x23,
            Self::Bgra8888 => 0x3,
            Self::Rgbx8888 => 0x22,
        }
    }

    /// The bytes the GPU writes per pixel.
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::A8 | Self::L8 => 1,
            Self::Rgba8888 | Self::Bgra8888 | Self::Rgbx8888 => 4,
        }
    }
}

/// A source's format (`convert_source_format` of the K230's build).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceFormat {
    /// One luma byte.
    L8,
    /// One alpha byte.
    A8,
    /// R, G, B, A bytes.
    Rgba8888,
    /// B, G, R, A bytes.
    Bgra8888,
    /// R, G, B, X bytes.
    Rgbx8888,
    /// One index byte, through the colour table.
    Index8,
}

impl SourceFormat {
    /// The format code of the source word.
    pub const fn code(self) -> u32 {
        match self {
            Self::L8 => 0x0,
            Self::A8 => 0x2,
            Self::Rgba8888 => 0x27,
            Self::Bgra8888 => 0x7,
            Self::Rgbx8888 => 0x26,
            Self::Index8 => 0x800,
        }
    }

    /// The bytes the GPU reads per pixel.
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::L8 | Self::A8 | Self::Index8 => 1,
            Self::Rgba8888 | Self::Bgra8888 | Self::Rgbx8888 => 4,
        }
    }
}

/// The source word's options beyond the format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceOptions {
    /// The bilinear filter (`filter_mode` 0x20000); point sampling otherwise.
    pub bilinear: bool,
    /// The colour-to-luma conversion of a colour source drawn to an L8 target (0x80000000).
    pub luma_conversion: bool,
    /// `ahb_read_split` (0x80): set when the blend reads the target.
    pub read_split: bool,
}

/// The layout of a render target or a source: dimensions and the stride in bytes. The bytes per
/// pixel come from the format it is used with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Width in pixels.
    pub width: u32,
    /// Height in rows.
    pub height: u32,
    /// Bytes from one row to the next.
    pub stride: u32,
}

impl Layout {
    /// Checks the layout, at `bytes_per_pixel`, against `buffer` and returns `stride * height`,
    /// the bytes it covers from the buffer's first byte.
    pub fn extent<B: Buffer + ?Sized>(&self, bytes_per_pixel: u32, buffer: &B) -> io::Result<u32> {
        if bytes_per_pixel == 0 || self.width == 0 || self.height == 0 {
            return Err(refused(format!("an empty layout {self:?}")));
        }
        if self.width > u32::from(u16::MAX) || self.height > u32::from(u16::MAX) {
            return Err(refused(format!(
                "{self:?} exceeds the 16-bit register fields"
            )));
        }
        if self.stride >= STRIDE_LIMIT {
            return Err(refused(format!(
                "stride {} reaches the stride register's tiling flag",
                self.stride
            )));
        }
        self.width
            .checked_mul(bytes_per_pixel)
            .filter(|&row| row <= self.stride)
            .ok_or_else(|| refused(format!("stride {} holds no row of {self:?}", self.stride)))?;
        let span = u64::from(self.stride) * u64::from(self.height);
        if span > buffer.byte_len() as u64 {
            return Err(refused(format!(
                "{span} bytes of {self:?} exceed the {}-byte buffer",
                buffer.byte_len()
            )));
        }
        if u64::from(buffer.gpu_address()) + span > 1 << 32 {
            return Err(refused(format!("{self:?} runs past the GPU address space")));
        }
        // `span` is at most `buffer.byte_len()` and below 2^32 by the check above.
        Ok(span as u32)
    }
}

/// The primitive mode the last configuration word chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// No configuration word yet.
    None,
    /// A clear of a rectangle (0x1).
    Rectangle,
    /// A pixel-engine clear (0x4).
    PeClear,
    /// An image draw: a rectangle sampled from the source (0x1001).
    Image,
    /// A path draw (neither).
    Path,
}

/// The render target set in a stream.
#[derive(Clone, Copy, Debug)]
struct Target {
    width: u32,
    height: u32,
    extent: u32,
}

/// A stream of command words, checked as it is built (see the module docs). A finished stream
/// runs with [`crate::Context::run`].
pub struct Stream<'a> {
    words: Vec<u32>,
    /// The capacity in words: one command buffer.
    capacity: usize,
    tessellation: Tessellation,
    target: Option<Target>,
    source: bool,
    mode: Mode,
    path_control: bool,
    window: bool,
    kernel_only: bool,
    finished: bool,
    buffers: PhantomData<&'a ()>,
}

impl std::fmt::Debug for Stream<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stream")
            .field("words", &self.words.len())
            .field("finished", &self.finished)
            .finish()
    }
}

impl<'a> Stream<'a> {
    /// An empty stream of at most `capacity` bytes (one command buffer), with `tessellation` as
    /// its only source of tessellation addresses. The words are allocated here, once: no method
    /// grows them past `capacity`.
    pub fn new(capacity: usize, tessellation: Tessellation) -> Self {
        let capacity = capacity / 4;
        Self {
            words: Vec::with_capacity(capacity),
            capacity,
            tessellation,
            target: None,
            source: false,
            mode: Mode::None,
            path_control: false,
            window: false,
            kernel_only: true,
            finished: false,
            buffers: PhantomData,
        }
    }

    /// This stream emptied, for new buffers: the words' allocation is kept.
    pub fn into_fresh<'b>(self) -> Stream<'b> {
        let mut words = self.words;
        words.clear();
        Stream {
            words,
            capacity: self.capacity,
            tessellation: self.tessellation,
            target: None,
            source: false,
            mode: Mode::None,
            path_control: false,
            window: false,
            kernel_only: true,
            finished: false,
            buffers: PhantomData,
        }
    }

    /// The words so far.
    pub fn words(&self) -> &[u32] {
        &self.words
    }

    /// The tessellation buffer the stream was made for.
    pub fn tessellation(&self) -> Tessellation {
        self.tessellation
    }

    /// Room for `n` more words, on a stream that is not finished.
    fn room(&self, n: usize) -> io::Result<()> {
        if self.finished {
            return Err(refused("the stream is finished".into()));
        }
        if self.words.len() + n > self.capacity {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!(
                    "{} more words do not fit the {}-byte command buffer",
                    n,
                    self.capacity * 4
                ),
            ));
        }
        Ok(())
    }

    fn state(&mut self, address: u32, value: u32) {
        self.words.extend_from_slice(&[STATE | address, value]);
    }

    fn target(&self) -> io::Result<Target> {
        self.target
            .ok_or_else(|| refused("no render target is set".into()))
    }

    /// Writes zero to one of the registers the vendor writes as zero (0x0A27, 0x0A2D, 0x0A34,
    /// 0x0A39, 0x0AD1). A zero 0x0A34 ends the path control.
    pub fn zero(&mut self, address: u32) -> io::Result<()> {
        if !ZERO_REGISTERS.contains(&address) {
            return Err(refused(format!(
                "register {address:#06x} is not one a stream zeroes"
            )));
        }
        self.room(2)?;
        if address == 0x0A34 {
            self.path_control = false;
        }
        self.state(address, 0);
        Ok(())
    }

    /// Sets the render target: `buffer` as `format` with `layout`, checked against the buffer
    /// (0x0A10 to 0x0A13). Later rectangles, clears and windows are checked against it.
    pub fn render_target<B: Buffer>(
        &mut self,
        buffer: &'a B,
        format: TargetFormat,
        layout: Layout,
    ) -> io::Result<()> {
        let extent = layout.extent(format.bytes_per_pixel(), buffer)?;
        self.room(8)?;
        self.kernel_only &= buffer.kernel_backed();
        self.state(0x0A10, format.code() | PREMULTIPLY_DST);
        self.state(0x0A11, buffer.gpu_address());
        self.state(0x0A12, layout.stride);
        self.state(0x0A13, layout.width | (layout.height << 16));
        self.target = Some(Target {
            width: layout.width,
            height: layout.height,
            extent,
        });
        self.window = false;
        Ok(())
    }

    /// Sets the source: its word (0x0A25, the format and `options`), then `buffer` with `layout`
    /// checked against it (0x0A27 to 0x0A2F). Zeroes 0x0A34, as the vendor does, which ends the
    /// path control.
    pub fn source<B: Buffer>(
        &mut self,
        buffer: &'a B,
        format: SourceFormat,
        layout: Layout,
        options: SourceOptions,
    ) -> io::Result<()> {
        layout.extent(format.bytes_per_pixel(), buffer)?;
        self.room(14)?;
        let word = format.code()
            | if options.bilinear { SOURCE_BILINEAR } else { 0 }
            | if options.luma_conversion {
                LUMA_CONVERSION
            } else {
                0
            }
            | if options.read_split { READ_SPLIT } else { 0 }
            | SRC_PREMULTIPLY;
        self.kernel_only &= buffer.kernel_backed();
        self.state(0x0A25, word);
        self.state(0x0A27, 0);
        self.state(0x0A29, buffer.gpu_address());
        self.state(0x0A34, 0);
        self.state(0x0A2B, layout.stride);
        self.state(0x0A2D, 0);
        self.state(0x0A2F, layout.width | (layout.height << 16));
        self.source = true;
        self.path_control = false;
        Ok(())
    }

    /// The configuration word (0x0A00): only [`CONFIG_BITS`], and one of the primitive modes
    /// (rectangle, pixel-engine clear, image, or none for a path).
    pub fn config(&mut self, word: u32) -> io::Result<()> {
        if word & !CONFIG_BITS != 0 {
            return Err(refused(format!(
                "configuration word {word:#010x} sets bits a stream may not"
            )));
        }
        let mode = match word & 0x1005 {
            0x0001 => Mode::Rectangle,
            0x0004 => Mode::PeClear,
            0x1001 => Mode::Image,
            0x0000 => Mode::Path,
            _ => {
                return Err(refused(format!(
                    "configuration word {word:#010x} names no primitive mode"
                )));
            }
        };
        self.room(2)?;
        self.state(0x0A00, word);
        self.mode = mode;
        Ok(())
    }

    /// The colour (0x0A02, `0xAABBGGRR`): of a clear, or a path draw; zero for a blit.
    pub fn colour(&mut self, colour: u32) -> io::Result<()> {
        self.room(2)?;
        self.state(0x0A02, colour);
        Ok(())
    }

    /// A blit's steps through the source (0x0A18 to 0x0A22, and 0x0A1F set to 1): the colour,
    /// x and y increments, each `[step, per row or column, offset]`, as floats.
    pub fn steps(&mut self, c: [f32; 3], x: [f32; 3], y: [f32; 3]) -> io::Result<()> {
        self.room(20)?;
        self.state(0x0A18, c[0].to_bits());
        self.state(0x0A19, c[1].to_bits());
        self.state(0x0A1A, c[2].to_bits());
        self.state(0x0A1C, x[0].to_bits());
        self.state(0x0A1D, x[1].to_bits());
        self.state(0x0A1E, x[2].to_bits());
        self.state(0x0A1F, 1);
        self.state(0x0A20, y[0].to_bits());
        self.state(0x0A21, y[1].to_bits());
        self.state(0x0A22, y[2].to_bits());
        Ok(())
    }

    /// The target flush (0x0A1B set to 1).
    pub fn target_flush(&mut self) -> io::Result<()> {
        self.room(2)?;
        self.state(0x0A1B, 1);
        Ok(())
    }

    /// A rectangle of the render target, (x, y) and `width` by `height`, inside the target's
    /// size, in the rectangle-clear mode, or in the image mode once a source is set.
    pub fn rectangle(&mut self, x: u32, y: u32, width: u32, height: u32) -> io::Result<()> {
        let target = self.target()?;
        match self.mode {
            Mode::Rectangle => {}
            Mode::Image if self.source => {}
            Mode::Image => return Err(refused("an image rectangle with no source set".into())),
            mode => {
                return Err(refused(format!("a rectangle in the {mode:?} mode")));
            }
        }
        inside(target, x, y, width, height, "rectangle")?;
        self.room(4)?;
        self.words
            .extend_from_slice(&[DATA | 1, 0, x | (y << 16), width | (height << 16)]);
        Ok(())
    }

    /// A pixel-engine clear of the first `bytes` bytes of the render target, in its mode. `bytes`
    /// may not exceed the target's extent.
    pub fn pe_clear(&mut self, bytes: u32) -> io::Result<()> {
        let target = self.target()?;
        if self.mode != Mode::PeClear {
            return Err(refused(format!(
                "a pixel-engine clear in the {:?} mode",
                self.mode
            )));
        }
        if bytes > target.extent {
            return Err(refused(format!(
                "a clear of {bytes} bytes is past the {}-byte target",
                target.extent
            )));
        }
        self.room(4)?;
        self.words.extend_from_slice(&[DATA | 1, 0, bytes, 0]);
        Ok(())
    }

    /// The 256-entry colour table at its fixed place (0x0B00), padded to an even count.
    pub fn clut(&mut self, colours: &[u32; 256]) -> io::Result<()> {
        self.room(258)?;
        self.words.push(CLUT_HEADER);
        self.words.extend_from_slice(colours);
        self.words.push(NOP);
        Ok(())
    }

    /// The tessellation buffer's address and sizes, from the context (`program_tessellation` of
    /// the K230's build: 0x0A35, 0x0AC8, 0x0ACB, 0x0ACC).
    pub fn tessellation_setup(&mut self) -> io::Result<()> {
        self.room(8)?;
        let tess = self.tessellation;
        let end = tess
            .physical
            .checked_add(tess.size)
            .ok_or_else(|| refused(format!("tessellation buffer {tess:?} wraps")))?;
        self.state(0x0A35, tess.physical);
        self.state(0x0AC8, tess.size);
        self.state(0x0ACB, end);
        self.state(0x0ACC, tess.count_size);
        Ok(())
    }

    /// The colour key's reset: 0x0A90 to 0x0A97 set to zero (`vg_lite_init`).
    pub fn colour_key_reset(&mut self) -> io::Result<()> {
        self.room(16)?;
        for key in 0x0A90..=0x0A97 {
            self.state(key, 0);
        }
        Ok(())
    }

    /// The path control word (0x0A34): the control bit and FP32 data (`0x01300000`), with only
    /// [`PATH_CONTROL_BITS`] beside them.
    pub fn path_control(&mut self, word: u32) -> io::Result<()> {
        if word & !PATH_CONTROL_BITS != PATH_CONTROL_BASE {
            return Err(refused(format!(
                "path control word {word:#010x} is not FP32 path control with the stroke and \
                 quality bits only"
            )));
        }
        self.room(2)?;
        self.state(0x0A34, word);
        self.path_control = true;
        Ok(())
    }

    /// The path's scale and bias (0x0A3B, 0x0A3C), its matrix (0x0A40 to 0x0A45) and the
    /// matrix's translation (0x0ACD, 0x0ACE), as floats.
    pub fn path_transform(
        &mut self,
        scale: f32,
        bias: f32,
        matrix: [f32; 6],
        translation: [f32; 2],
    ) -> io::Result<()> {
        self.room(20)?;
        self.state(0x0A3B, scale.to_bits());
        self.state(0x0A3C, bias.to_bits());
        for (i, value) in matrix.iter().enumerate() {
            self.state(0x0A40 + i as u32, value.to_bits());
        }
        self.state(0x0ACD, translation[0].to_bits());
        self.state(0x0ACE, translation[1].to_bits());
        Ok(())
    }

    /// The tessellation window of a path draw (one window, as `vg_lite_draw` writes it): 0x0A1B
    /// as `0x00011000`, the tessellation buffer's size in 64-byte units (0x0A3D, from the
    /// context), and the window's origin and size (0x0A39, 0x0A3A), inside the render target.
    pub fn tessellation_window(
        &mut self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> io::Result<()> {
        let target = self.target()?;
        inside(target, x, y, width, height, "window")?;
        self.room(8)?;
        let tess = self.tessellation;
        self.state(0x0A1B, 0x0001_1000);
        self.state(0x0A3D, tess.size / 64);
        self.state(0x0A39, x | (y << 16));
        self.state(0x0A3A, width | (height << 16));
        self.window = true;
        Ok(())
    }

    /// Inline path data (`push_data`): a data header of the padded length in eight-byte units,
    /// a zero word, the bytes as little-endian words, then zero words to the padded length.
    /// `data` is whole words and not empty, and needs the path mode, a tessellation window and
    /// the path control set in this stream.
    pub fn path_data(&mut self, data: &[u8]) -> io::Result<()> {
        if data.is_empty() || !data.len().is_multiple_of(4) {
            return Err(refused(format!(
                "{} bytes of inline data: whole words only",
                data.len()
            )));
        }
        self.target()?;
        if self.mode != Mode::Path || !self.window || !self.path_control {
            return Err(refused(
                "path data needs the path mode, a tessellation window and the path control".into(),
            ));
        }
        let padded = data.len().next_multiple_of(8);
        self.room(2 + padded / 4)?;
        self.words.push(DATA | (padded / 8) as u32);
        self.words.push(0);
        self.words.extend(
            data.as_chunks::<4>()
                .0
                .iter()
                .map(|w| u32::from_le_bytes(*w)),
        );
        // The vendor zeroes the last eight bytes at the padded length, and the data then
        // overwrites them up to its end: one zero word when the data is an odd number of words.
        self.words
            .resize(self.words.len() + (padded - data.len()) / 4, 0);
        Ok(())
    }

    /// The pixel engine's flush: the target flushed, then a semaphore and a stall on module 7.
    pub fn flush(&mut self) -> io::Result<()> {
        self.room(6)?;
        self.state(0x0A1B, 1);
        self.words.extend_from_slice(&[SEMAPHORE_7, 0, STALL_7, 0]);
        Ok(())
    }

    /// Ends the stream: the flush, then the end command. Nothing may be added after.
    pub fn finish(&mut self) -> io::Result<()> {
        self.room(8)?;
        self.flush()?;
        self.words.extend_from_slice(&[0, 0]);
        self.finished = true;
        Ok(())
    }

    /// Whether [`crate::Context::run`] may submit this stream: it is finished, it names only
    /// kernel memory, and it fits one command buffer of `capacity` bytes.
    pub(crate) fn submittable(&self, capacity: usize) -> io::Result<()> {
        if !self.finished {
            return Err(refused("the stream is not finished".into()));
        }
        if !self.kernel_only {
            return Err(refused(
                "the stream names a buffer the kernel does not own".into(),
            ));
        }
        let bytes = size_of_val(self.words.as_slice());
        if bytes > capacity {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!("{bytes} bytes of commands for a {capacity}-byte command buffer"),
            ));
        }
        Ok(())
    }
}

/// A rectangle (x, y) `width` by `height`, non-empty and inside `target`.
fn inside(target: Target, x: u32, y: u32, width: u32, height: u32, what: &str) -> io::Result<()> {
    let ok = width > 0
        && height > 0
        && x.checked_add(width).is_some_and(|r| r <= target.width)
        && y.checked_add(height).is_some_and(|b| b <= target.height);
    if ok {
        Ok(())
    } else {
        Err(refused(format!(
            "{what} {x},{y} {width}x{height} is outside the {}x{} target",
            target.width, target.height
        )))
    }
}

fn refused(why: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why)
}
