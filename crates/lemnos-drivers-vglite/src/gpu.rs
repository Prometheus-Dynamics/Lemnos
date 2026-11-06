//! [`Vglite`]: the opened GPU and one call per operation.
//!
//! The vendor library keeps one context per process and runs one call at a time, so the context
//! is opened once and every operation takes its lock. Each operation is one submit: its stream
//! (the target, the operation, the flush and the end; see `command.rs`), then a wait, then the
//! result copied out of a staging buffer when there was one. The open follows `vg_lite_init` and
//! the flow follows `vg_lite_finish` of `VGLite/vg_lite.c` (Vivante, MIT; notice in `command.rs`).

use std::io;
use std::sync::{Mutex, MutexGuard, OnceLock};

use lemnos_drivers_rga::{Filter, Transform};
use lemnos_linux_sys::vg_lite::{Context, Device, Identification};

use crate::command::{self, Op};
use crate::plan::{
    self, BLEND_NONE, BlendMode, Format, IDENTITY, Image, Matrix, Sampling, check_output,
    check_rect, invalid, unsupported,
};
use crate::surface::{Frame, Input, Output, Surface, Transfer};

/// `VG_LITE_COMMAND_BUFFER_SIZE`: the bytes of each command buffer (128 KiB).
const COMMAND_BUFFER_BYTES: u32 = 128 << 10;
/// The tessellation buffer `vg_lite_init(256, 256)` asks for.
const TESS: i32 = 256;
/// The chip identification registers the library reads at initialize (`get_product_info`, and
/// the CID at 0x30), and the values `vg_lite_options.h` builds for: CHIPID 0x265, REVISION
/// 0x1003, CID 0x417 (unverified on board).
const CHIP_ID: u32 = 0x265;
const REVISION: u32 = 0x1003;
const CID: u32 = 0x417;

/// The one context of the process, opened on the first [`Vglite::load`].
static GPU: OnceLock<Result<Mutex<Context>, String>> = OnceLock::new();

/// `libvg_lite`'s context and one call per operation, on `/dev/vg_lite`. Untested on board: see
/// the crate docs.
#[derive(Clone, Copy)]
pub struct Vglite {
    gpu: &'static Mutex<Context>,
}

impl Vglite {
    /// Open `/dev/vg_lite`, check the chip, and set the context up (`vg_lite_init(256, 256)`).
    /// The first call does the work; later calls share the context.
    pub fn load() -> io::Result<Self> {
        let loaded = GPU.get_or_init(|| open().map(Mutex::new).map_err(|e| e.to_string()));
        match loaded {
            Ok(gpu) => Ok(Self { gpu }),
            Err(why) => Err(io::Error::new(io::ErrorKind::NotFound, why.clone())),
        }
    }

    fn context(&self) -> MutexGuard<'static, Context> {
        self.gpu
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// One operation: the surfaces it reads and writes, its stream built from their buffers (the
    /// stream checks every address and extent), the stream run and waited for, and the result
    /// copied out. An operation that draws nothing submits nothing and leaves the output alone.
    fn run(
        &self,
        src: Option<(Image, Input<'_>)>,
        dst: Image,
        mut out: Output<'_>,
        keep: bool,
        op: Op<'_>,
    ) -> io::Result<Transfer> {
        let mut gpu = self.context();
        let device = gpu.device().clone();
        let mut transfer = Transfer::default();
        let mut source = match &src {
            Some((image, input)) => Some(Surface::input(&device, *image, input, &mut transfer)?),
            None => None,
        };
        let mut target = Surface::output(&device, dst, &mut out, keep, &mut transfer)?;
        if let Some(source) = source.as_mut() {
            source.clean_for_gpu()?;
        }
        let mut stream = gpu.stream();
        let source_bound = source.as_ref().map(Surface::bound);
        if !command::encode(op, &mut stream, target.bound(), source_bound)? {
            return Ok(transfer);
        }
        stream.finish()?;
        gpu.run(stream)?;
        target.sync_after_gpu()?;
        target.download(&mut out, &mut transfer)?;
        Ok(transfer)
    }

    /// A blit of the whole source through `matrix`, with `blend` and `sampling`.
    #[allow(clippy::too_many_arguments)]
    fn blit(
        &self,
        src: (Image, Input<'_>),
        dst: Image,
        out: Output<'_>,
        keep: bool,
        matrix: Matrix,
        blend: u32,
        sampling: Sampling,
    ) -> io::Result<Transfer> {
        self.run(
            Some(src),
            dst,
            out,
            keep,
            Op::Blit {
                matrix,
                blend,
                sampling,
            },
        )
    }

    fn resize(
        &self,
        src: Image,
        input: Input<'_>,
        dst: Image,
        out: Output<'_>,
        filter: Filter,
    ) -> io::Result<Transfer> {
        let (matrix, sampling) = plan::resize(src, dst, filter)?;
        // An area downscale reads whole 2x2 blocks: a trailing odd row or column is cropped.
        let src = match filter {
            Filter::Area => Image {
                width: dst.width * 2,
                height: dst.height * 2,
                ..src
            },
            Filter::Bilinear => src,
        };
        self.blit((src, input), dst, out, false, matrix, BLEND_NONE, sampling)
    }

    /// Resize between byte buffers of one format, to `dst`'s size: [`Filter::Bilinear`] either
    /// way, [`Filter::Area`] by exactly 2 (a half-phase bilinear blit; anything else is
    /// `Unsupported`).
    pub fn resize_bytes(
        &self,
        src: Image,
        from: &[u8],
        dst: Image,
        to: &mut [u8],
        filter: Filter,
    ) -> io::Result<Transfer> {
        src.check(from.len())?;
        dst.check(to.len())?;
        self.resize(src, Input::Bytes(from), dst, Output::Bytes(to), filter)
    }

    /// [`Vglite::resize_bytes`] between dmabufs.
    pub fn resize_dmabuf(
        &self,
        src: Frame<'_>,
        dst: Frame<'_>,
        filter: Filter,
    ) -> io::Result<Transfer> {
        self.resize(
            src.image,
            Input::Frame(src),
            dst.image,
            Output::Frame(dst),
            filter,
        )
    }

    fn crop(
        &self,
        src: Image,
        input: Input<'_>,
        rect: (u32, u32, u32, u32),
        dst: Image,
        out: Output<'_>,
    ) -> io::Result<Transfer> {
        check_rect(rect, src.width, src.height)?;
        check_output(dst, (rect.2, rect.3), src.format)?;
        self.blit(
            (src, input),
            dst,
            out,
            false,
            plan::crop(rect),
            BLEND_NONE,
            Sampling::Point,
        )
    }

    /// `rect` (left, top, width, height) of `src`, unscaled, into `dst` of the rectangle's size:
    /// a translated, point-sampled blit. A full-frame rectangle is a copy.
    pub fn crop_bytes(
        &self,
        src: Image,
        from: &[u8],
        rect: (u32, u32, u32, u32),
        dst: Image,
        to: &mut [u8],
    ) -> io::Result<Transfer> {
        src.check(from.len())?;
        dst.check(to.len())?;
        self.crop(src, Input::Bytes(from), rect, dst, Output::Bytes(to))
    }

    /// [`Vglite::crop_bytes`] between dmabufs.
    pub fn crop_dmabuf(
        &self,
        src: Frame<'_>,
        rect: (u32, u32, u32, u32),
        dst: Frame<'_>,
    ) -> io::Result<Transfer> {
        self.crop(
            src.image,
            Input::Frame(src),
            rect,
            dst.image,
            Output::Frame(dst),
        )
    }

    fn transform(
        &self,
        src: Image,
        input: Input<'_>,
        dst: Image,
        out: Output<'_>,
        t: Transform,
    ) -> io::Result<Transfer> {
        check_output(dst, t.output_size(src.width, src.height), src.format)?;
        self.blit(
            (src, input),
            dst,
            out,
            false,
            plan::transform(t, src.width, src.height),
            BLEND_NONE,
            Sampling::Point,
        )
    }

    /// A flip or a clockwise quarter turn into `dst` of the result's size: a mirror or rotation
    /// matrix, point sampled (the K230's VGLite has no mirror shortcut).
    pub fn transform_bytes(
        &self,
        src: Image,
        from: &[u8],
        dst: Image,
        to: &mut [u8],
        t: Transform,
    ) -> io::Result<Transfer> {
        src.check(from.len())?;
        dst.check(to.len())?;
        self.transform(src, Input::Bytes(from), dst, Output::Bytes(to), t)
    }

    /// [`Vglite::transform_bytes`] between dmabufs.
    pub fn transform_dmabuf(
        &self,
        src: Frame<'_>,
        dst: Frame<'_>,
        t: Transform,
    ) -> io::Result<Transfer> {
        self.transform(
            src.image,
            Input::Frame(src),
            dst.image,
            Output::Frame(dst),
            t,
        )
    }

    fn fill(
        &self,
        image: Image,
        out: Output<'_>,
        rect: (u32, u32, u32, u32),
        rgba: [u8; 4],
    ) -> io::Result<Transfer> {
        check_rect(rect, image.width, image.height)?;
        if matches!(image.format, Format::L8 | Format::Index8) {
            return unsupported(format!(
                "fill of {:?}: VGLite converts the colour to luma with a truncating float sum; \
                 fill a mask as A8",
                image.format
            ));
        }
        self.run(
            None,
            image,
            out,
            true,
            Op::Clear {
                rect,
                color: plan::color(rgba),
            },
        )
    }

    /// Set `rect` (left, top, width, height) to `rgba` (`vg_lite_clear`). An A8 mask takes the
    /// alpha byte. L8 is `Unsupported`.
    pub fn fill_bytes(
        &self,
        image: Image,
        buf: &mut [u8],
        rect: (u32, u32, u32, u32),
        rgba: [u8; 4],
    ) -> io::Result<Transfer> {
        image.check(buf.len())?;
        self.fill(image, Output::Bytes(buf), rect, rgba)
    }

    /// [`Vglite::fill_bytes`] on a dmabuf.
    pub fn fill_dmabuf(
        &self,
        frame: Frame<'_>,
        rect: (u32, u32, u32, u32),
        rgba: [u8; 4],
    ) -> io::Result<Transfer> {
        self.fill(frame.image, Output::Frame(frame), rect, rgba)
    }

    fn blend(
        &self,
        fg: Image,
        input: Input<'_>,
        dst: Image,
        out: Output<'_>,
        mode: BlendMode,
        premultiplied: bool,
    ) -> io::Result<Transfer> {
        check_output(dst, (fg.width, fg.height), fg.format)?;
        if !matches!(fg.format, Format::Rgba8888 | Format::Bgra8888) {
            return unsupported(format!("blend of {:?}: RGBA or BGRA only", fg.format));
        }
        let blend = plan::blend(mode, premultiplied)?;
        self.blit(
            (fg, input),
            dst,
            out,
            true,
            IDENTITY,
            blend,
            Sampling::Point,
        )
    }

    /// `fg` over (or under) `dst` of the same size and format, in place: SRC, SRC_OVER or
    /// DST_OVER with straight alpha. Premultiplied SRC_OVER and DST_OVER are `Unsupported`.
    pub fn blend_bytes(
        &self,
        fg: Image,
        fg_bytes: &[u8],
        dst: Image,
        dst_bytes: &mut [u8],
        mode: BlendMode,
        premultiplied: bool,
    ) -> io::Result<Transfer> {
        fg.check(fg_bytes.len())?;
        dst.check(dst_bytes.len())?;
        self.blend(
            fg,
            Input::Bytes(fg_bytes),
            dst,
            Output::Bytes(dst_bytes),
            mode,
            premultiplied,
        )
    }

    /// [`Vglite::blend_bytes`] between dmabufs.
    pub fn blend_dmabuf(
        &self,
        fg: Frame<'_>,
        dst: Frame<'_>,
        mode: BlendMode,
        premultiplied: bool,
    ) -> io::Result<Transfer> {
        self.blend(
            fg.image,
            Input::Frame(fg),
            dst.image,
            Output::Frame(dst),
            mode,
            premultiplied,
        )
    }

    fn palette(
        &self,
        indices: Image,
        input: Input<'_>,
        lut: &[[u8; 4]; 256],
        dst: Image,
        out: Output<'_>,
    ) -> io::Result<Transfer> {
        if indices.format != Format::Index8 {
            return invalid(format!(
                "palette indices as {:?}, not Index8",
                indices.format
            ));
        }
        check_output(dst, (indices.width, indices.height), Format::Rgba8888)?;
        let colours: [u32; 256] = std::array::from_fn(|i| plan::color(lut[i]));
        self.run(
            Some((indices, input)),
            dst,
            out,
            false,
            Op::Palette { colours: &colours },
        )
    }

    /// 8-bit indices to RGBA8 through 256 entries (an `INDEX_8` source and the colour table),
    /// tightly packed byte buffers.
    pub fn palette_bytes(
        &self,
        width: u32,
        height: u32,
        indices: &[u8],
        lut: &[[u8; 4]; 256],
        dst: &mut [u8],
    ) -> io::Result<Transfer> {
        let (s, d) = (
            Image::packed(Format::Index8, width, height),
            Image::packed(Format::Rgba8888, width, height),
        );
        s.check(indices.len())?;
        d.check(dst.len())?;
        self.palette(s, Input::Bytes(indices), lut, d, Output::Bytes(dst))
    }

    /// [`Vglite::palette_bytes`] between dmabufs (`indices` as [`Format::Index8`]).
    pub fn palette_dmabuf(
        &self,
        indices: Frame<'_>,
        lut: &[[u8; 4]; 256],
        dst: Frame<'_>,
    ) -> io::Result<Transfer> {
        self.palette(
            indices.image,
            Input::Frame(indices),
            lut,
            dst.image,
            Output::Frame(dst),
        )
    }

    /// A one-pixel line from `from` to `to`, both ends included, in `rgba`. `Unsupported` until
    /// the stroke port (see `TODO.md`).
    pub fn line_bytes(
        &self,
        image: Image,
        buf: &mut [u8],
        from: (i32, i32),
        to: (i32, i32),
        rgba: [u8; 4],
    ) -> io::Result<Transfer> {
        image.check(buf.len())?;
        let _ = (from, to, rgba);
        stroke_unsupported()
    }

    /// [`Vglite::line_bytes`] on a dmabuf: `Unsupported` until the stroke port.
    pub fn line_dmabuf(
        &self,
        frame: Frame<'_>,
        from: (i32, i32),
        to: (i32, i32),
        rgba: [u8; 4],
    ) -> io::Result<Transfer> {
        let _ = (frame, from, to, rgba);
        stroke_unsupported()
    }
}

fn stroke_unsupported() -> io::Result<Transfer> {
    unsupported(
        "a line is a stroke: the VGLite stroke port (phase 2) is not in this crate; draw on the CPU"
            .into(),
    )
}

/// Open the device, initialize the context, check the chip, and run the set-up stream.
fn open() -> io::Result<Context> {
    let device = Device::open()?;
    let mut context = device.initialize(COMMAND_BUFFER_BYTES, TESS, TESS)?;
    let id = device.identification(Identification::ChipId)?;
    let revision = device.identification(Identification::Revision)?;
    let cid = device.identification(Identification::Cid)?;
    if (id, revision, cid) != (CHIP_ID, REVISION, CID) {
        return unsupported(format!(
            "not the K230's VGLite: id {id:#x}, revision {revision:#x}, cid {cid:#x}"
        ));
    }
    let mut stream = context.stream();
    command::init(&mut stream)?;
    stream.finish()?;
    context.run(stream)?;
    Ok(context)
}
