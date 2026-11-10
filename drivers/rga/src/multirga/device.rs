//! [`MultiRga`]: one `rga_req` per operation, sent with `RGA_BLIT_SYNC` on `/dev/rga`. Each
//! operation first builds its request as a [`Job`] (no device touched, so the field mappings are
//! tested on any host), then runs it. Every image is a dma-buf the caller holds; the job borrows
//! its descriptor until the call returns, and checks that the dma-buf holds the image.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};

use lemnos_driver_rga_sys::request::{
    self as rga, CscCoe, FullCsc, ImgInfo, Job, Mode, PorterDuff, Rect as RgaRect, RopCode, Turn,
    YuvMode,
};

use super::{BlendMode, CHANNEL_COEFFICIENT, CHANNEL_OFFSET, Channel, Image, RgaFormat, Rop};
use crate::Transform;

/// The device node.
pub const NODE: &str = "/dev/rga";

/// The multi-RGA node, open for requests. See the module docs and the crate docs for what each
/// operation's route covers and which field mappings are unverified.
#[derive(Debug)]
pub struct MultiRga {
    file: File,
    path: PathBuf,
}

impl MultiRga {
    /// Opens `/dev/rga` read-write, when it exists and opens.
    pub fn find() -> Option<Self> {
        Self::open(Path::new(NODE)).ok()
    }

    /// Opens the multi-RGA node at `path`.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// The device node.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn run(&self, job: &mut Job<'_>) -> io::Result<()> {
        rga::blit_sync(self.file.as_fd(), job)
    }

    /// Copies `src` into `dst`, both of `image` (a plain blit: no conversion, blend or turn).
    pub fn copy(&self, image: Image, src: BorrowedFd<'_>, dst: BorrowedFd<'_>) -> io::Result<()> {
        self.run(&mut copy_job(image, src, dst)?)
    }

    /// Fills `rect` (left, top, width, height) of `dst` with `rgba` (a solid colour fill).
    /// Field mapping unverified: the colour's byte order (`0xAABBGGRR` as the crate packs it),
    /// and whether the active rectangle is the fill area.
    pub fn fill(
        &self,
        image: Image,
        dst: BorrowedFd<'_>,
        rect: (u32, u32, u32, u32),
        rgba: [u8; 4],
    ) -> io::Result<()> {
        self.run(&mut fill_job(image, dst, rect, rgba)?)
    }

    /// `dst = src op dst` on two 8-bit masks of `width` by `height` (`width` a multiple of four:
    /// the masks go as RGBA8888 at a quarter of the width). Field mapping unverified: the ROP
    /// codes and whether `dst` is the destination of the ROP as well as its target.
    pub fn rop(
        &self,
        width: u32,
        height: u32,
        src: BorrowedFd<'_>,
        dst: BorrowedFd<'_>,
        op: Rop,
    ) -> io::Result<()> {
        self.run(&mut rop_job(width, height, src, dst, op)?)
    }

    /// Blends `fg` onto `dst` in place with `mode`, straight alpha or `premultiplied`. Field
    /// mapping unverified: the Porter-Duff values (see [`BlendMode::pd_code`]) and the
    /// premultiplied flag's encoding.
    pub fn blend(
        &self,
        image: Image,
        fg: BorrowedFd<'_>,
        dst: BorrowedFd<'_>,
        mode: BlendMode,
        premultiplied: bool,
    ) -> io::Result<()> {
        self.run(&mut blend_job(image, fg, dst, mode, premultiplied)?)
    }

    /// The pixels of `src` (R, G, B, X) whose every channel lies in `min..=max` drawn opaque onto
    /// `dst`, which the caller clears first (its alpha is then the mask). Field mapping
    /// unverified: the header has the colour key range but not its enable bit or inversion mode,
    /// so the request sends the range alone.
    pub fn color_key(
        &self,
        width: u32,
        height: u32,
        src: BorrowedFd<'_>,
        min: [u8; 3],
        max: [u8; 3],
        dst: BorrowedFd<'_>,
    ) -> io::Result<()> {
        self.run(&mut color_key_job(width, height, src, min, max, dst)?)
    }

    /// Gray (Y only, full range) to RGBA8888 with alpha opaque, the same size. Field mapping
    /// unverified: the chroma of a Y-only source is taken as neutral.
    pub fn gray_to_rgba(
        &self,
        width: u32,
        height: u32,
        src: BorrowedFd<'_>,
        dst: BorrowedFd<'_>,
    ) -> io::Result<()> {
        self.run(&mut gray_job(width, height, src, dst)?)
    }

    /// One channel of an RGBA8888 image to gray (Y only, `RK_FORMAT_YCbCr_400`), the same size:
    /// `dst[i] = src[4 * i + c]` for the channel's byte `c`, meant to be exact, not luma.
    ///
    /// It runs through the request's user colour-space conversion (`full_csc`), which only RGA2
    /// cores have; on an RK3588 the kernel's scheduler gives it to the RGA2 core. The Y row is
    /// 1023/1024 on the channel and zero on the others, plus an offset of 383/1024, which is
    /// exact for every byte whether the hardware truncates or rounds; the clip is the kernel's
    /// default full range (0 to 255). Field mapping unverified: which coefficient meets the first
    /// byte of RGBA8888 (the vendor table names it R), the coefficient register's width, and the
    /// hardware's rounding. Untested on board; [`crate::caps`] does not route it until the board
    /// test matches.
    pub fn channel_to_gray(
        &self,
        width: u32,
        height: u32,
        channel: Channel,
        src: BorrowedFd<'_>,
        dst: BorrowedFd<'_>,
    ) -> io::Result<()> {
        self.run(&mut channel_job(width, height, channel, src, dst)?)
    }

    /// Flips or turns `src` into `dst` (the output size of `transform`). Field mapping
    /// unverified: which mirror is horizontal, and the direction of a quarter turn (clockwise is
    /// assumed).
    pub fn transform(
        &self,
        src: Image,
        input: BorrowedFd<'_>,
        dst: Image,
        output: BorrowedFd<'_>,
        transform: Transform,
    ) -> io::Result<()> {
        self.run(&mut transform_job(src, input, dst, output, transform)?)
    }
}

/// Where a built request goes: the node, or a test's stand-in that sees each request as it would
/// leave. Every request reaching it was built by this module's builders, so it carries their
/// checks (each image against its dma-buf's real size).
pub(crate) trait Submit: Send {
    fn submit(&self, job: &mut Job<'_>) -> io::Result<()>;
}

impl Submit for MultiRga {
    fn submit(&self, job: &mut Job<'_>) -> io::Result<()> {
        self.run(job)
    }
}

/// [`MultiRga::copy`] on any [`Submit`].
pub(crate) fn copy_on(
    device: &dyn Submit,
    image: Image,
    src: BorrowedFd<'_>,
    dst: BorrowedFd<'_>,
) -> io::Result<()> {
    device.submit(&mut copy_job(image, src, dst)?)
}

/// [`MultiRga::blend`] on any [`Submit`].
pub(crate) fn blend_on(
    device: &dyn Submit,
    image: Image,
    fg: BorrowedFd<'_>,
    dst: BorrowedFd<'_>,
    mode: BlendMode,
    premultiplied: bool,
) -> io::Result<()> {
    device.submit(&mut blend_job(image, fg, dst, mode, premultiplied)?)
}

/// [`MultiRga::gray_to_rgba`] on any [`Submit`].
pub(crate) fn gray_on(
    device: &dyn Submit,
    width: u32,
    height: u32,
    src: BorrowedFd<'_>,
    dst: BorrowedFd<'_>,
) -> io::Result<()> {
    device.submit(&mut gray_job(width, height, src, dst)?)
}

/// The request's dimension fields are 16-bit.
fn dim(v: u32) -> io::Result<u16> {
    u16::try_from(v).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{v} pixels is more than the request's 16-bit dimensions"),
        )
    })
}

/// The image description of `image` for the request (no addresses: the job sets them).
fn info(image: Image) -> io::Result<ImgInfo> {
    Ok(ImgInfo {
        format: image.format.code() as u32,
        act_w: dim(image.width)?,
        act_h: dim(image.height)?,
        vir_w: dim(image.wstride)?,
        vir_h: dim(image.hstride)?,
        enable: 1,
        ..ImgInfo::default()
    })
}

/// The full destination clip (the header's default is the destination's virtual size).
fn full_clip(image: Image) -> io::Result<RgaRect> {
    Ok(RgaRect {
        xmin: 0,
        xmax: dim(image.width)?.saturating_sub(1),
        ymin: 0,
        ymax: dim(image.height)?.saturating_sub(1),
    })
}

/// `rect` (left, top, width, height) is non-empty and inside `image`.
fn check_rect(rect: (u32, u32, u32, u32), image: Image) -> io::Result<()> {
    let (left, top, w, h) = rect;
    let inside = w > 0
        && h > 0
        && left.checked_add(w).is_some_and(|r| r <= image.width)
        && top.checked_add(h).is_some_and(|b| b <= image.height);
    if inside {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "rectangle {rect:?} is not inside {}x{}",
                image.width, image.height
            ),
        ))
    }
}

/// The request of a solid fill.
fn fill_job(
    image: Image,
    dst: BorrowedFd<'_>,
    rect: (u32, u32, u32, u32),
    rgba: [u8; 4],
) -> io::Result<Job<'_>> {
    check_rect(rect, image)?;
    let mut target = info(image)?;
    target.x_offset = dim(rect.0)?;
    target.y_offset = dim(rect.1)?;
    target.act_w = dim(rect.2)?;
    target.act_h = dim(rect.3)?;
    let mut job = Job::new(Mode::Fill);
    job.set_dst(target, dst)?;
    job.set_fill_color(u32::from_le_bytes(rgba));
    job.set_clip(full_clip(image)?);
    Ok(job)
}

/// The request of a plain copy of `image` from `src` to `dst`.
fn copy_job<'a>(image: Image, src: BorrowedFd<'a>, dst: BorrowedFd<'a>) -> io::Result<Job<'a>> {
    let mut job = Job::new(Mode::Blit);
    job.set_src(info(image)?, src)?;
    job.set_dst(info(image)?, dst)?;
    job.set_clip(full_clip(image)?);
    Ok(job)
}

/// The request of a mask ROP: masks are RGBA8888 at a quarter of the width.
fn rop_job<'a>(
    width: u32,
    height: u32,
    src: BorrowedFd<'a>,
    dst: BorrowedFd<'a>,
    op: Rop,
) -> io::Result<Job<'a>> {
    if width == 0 || height == 0 || !width.is_multiple_of(4) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("mask {width}x{height}: the width must be a non-zero multiple of four"),
        ));
    }
    let mask = Image::packed(RgaFormat::Rgba8888, width / 4, height);
    let mut job = Job::new(Mode::Blit);
    job.set_src(info(mask)?, src)?;
    job.set_dst(info(mask)?, dst)?;
    job.set_rop(match op {
        Rop::And => RopCode::And,
        Rop::Or => RopCode::Or,
        Rop::Xor => RopCode::Xor,
    });
    Ok(job)
}

/// The request of a Porter-Duff blend of `fg` onto `dst`.
fn blend_job<'a>(
    image: Image,
    fg: BorrowedFd<'a>,
    dst: BorrowedFd<'a>,
    mode: BlendMode,
    premultiplied: bool,
) -> io::Result<Job<'a>> {
    let mut job = Job::new(Mode::Blit);
    job.set_src(info(image)?, fg)?;
    job.set_dst(info(image)?, dst)?;
    job.set_clip(full_clip(image)?);
    job.set_blend(
        match mode {
            BlendMode::Src => PorterDuff::Src,
            BlendMode::SrcOver => PorterDuff::SrcOver,
            BlendMode::DstOver => PorterDuff::DstOver,
        },
        premultiplied,
    );
    Ok(job)
}

/// The request of a colour-key range onto RGBA.
fn color_key_job<'a>(
    width: u32,
    height: u32,
    src: BorrowedFd<'a>,
    min: [u8; 3],
    max: [u8; 3],
    dst: BorrowedFd<'a>,
) -> io::Result<Job<'a>> {
    let input = Image::packed(RgaFormat::Rgbx8888, width, height);
    let out = Image::packed(RgaFormat::Rgba8888, width, height);
    let mut job = Job::new(Mode::Blit);
    job.set_src(info(input)?, src)?;
    job.set_dst(info(out)?, dst)?;
    job.set_color_key(
        u32::from_le_bytes([min[0], min[1], min[2], 0]),
        u32::from_le_bytes([max[0], max[1], max[2], 0xff]),
    );
    Ok(job)
}

/// The request of a gray to RGBA conversion.
fn gray_job<'a>(
    width: u32,
    height: u32,
    src: BorrowedFd<'a>,
    dst: BorrowedFd<'a>,
) -> io::Result<Job<'a>> {
    let input = Image::packed(RgaFormat::Gray8, width, height);
    let out = Image::packed(RgaFormat::Rgba8888, width, height);
    let mut job = Job::new(Mode::Blit);
    job.set_src(info(input)?, src)?;
    job.set_dst(info(out)?, dst)?;
    job.set_yuv_mode(YuvMode::Bt601Full);
    Ok(job)
}

/// The request of a channel to gray conversion: Y only, through the user conversion.
fn channel_job<'a>(
    width: u32,
    height: u32,
    channel: Channel,
    src: BorrowedFd<'a>,
    dst: BorrowedFd<'a>,
) -> io::Result<Job<'a>> {
    let input = Image::packed(RgaFormat::Rgba8888, width, height);
    let out = Image::packed(RgaFormat::Gray8, width, height);
    let c = CHANNEL_COEFFICIENT;
    let (r_v, g_y, b_u) = match channel {
        Channel::Red => (c, 0, 0),
        Channel::Green => (0, c, 0),
        Channel::Blue => (0, 0, c),
    };
    let mut job = Job::new(Mode::Blit);
    job.set_src(info(input)?, src)?;
    job.set_dst(info(out)?, dst)?;
    job.set_full_csc(FullCsc {
        flag: 1,
        coe_y: CscCoe {
            r_v,
            g_y,
            b_u,
            off: CHANNEL_OFFSET,
        },
        // A Y-only destination has no chroma; these rows are written and unused.
        coe_u: CscCoe::default(),
        coe_v: CscCoe::default(),
    });
    Ok(job)
}

/// The request of a flip or quarter turn: the rotate mode and, for turns, `sina`/`cosa`.
fn transform_job<'a>(
    src: Image,
    input: BorrowedFd<'a>,
    dst: Image,
    output: BorrowedFd<'a>,
    transform: Transform,
) -> io::Result<Job<'a>> {
    let want = transform.output_size(src.width, src.height);
    if (dst.width, dst.height) != want {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "a {}x{} output for a {}x{} result",
                dst.width, dst.height, want.0, want.1
            ),
        ));
    }
    let mut job = Job::new(Mode::Blit);
    job.set_src(info(src)?, input)?;
    job.set_dst(info(dst)?, output)?;
    job.set_clip(full_clip(dst)?);
    job.set_turn(match transform {
        Transform::FlipHorizontal => Turn::MirrorX,
        Transform::FlipVertical => Turn::MirrorY,
        Transform::Rotate90 => Turn::Rotate90,
        Transform::Rotate180 => Turn::Rotate180,
        Transform::Rotate270 => Turn::Rotate270,
    });
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    /// A memfd of `len` bytes: it answers the size query as a dma-buf does.
    fn buffer(len: u64) -> File {
        let file = File::from(lemnos_linux_sys::dma_buf::memfd(c"rga-test").unwrap());
        file.set_len(len).unwrap();
        file
    }

    #[test]
    fn a_channel_conversion_programs_the_y_row_only() {
        let (src, dst) = (buffer(8 * 2 * 4), buffer(8 * 2));
        for (channel, row) in [
            (Channel::Red, (1023, 0, 0)),
            (Channel::Green, (0, 1023, 0)),
            (Channel::Blue, (0, 0, 1023)),
        ] {
            let job = channel_job(8, 2, channel, src.as_fd(), dst.as_fd()).unwrap();
            let req = job.request();
            assert_eq!(req.render_mode, 0);
            assert_eq!(req.yuv2rgb_mode, 0);
            let csc = req.full_csc;
            assert_eq!(csc.flag, 1);
            assert_eq!((csc.coe_y.r_v, csc.coe_y.g_y, csc.coe_y.b_u), row);
            assert_eq!(csc.coe_y.off, CHANNEL_OFFSET);
            assert_eq!(csc.coe_u, CscCoe::default());
            assert_eq!(req.src().yrgb_addr, src.as_raw_fd() as u64);
            assert_eq!(req.dst().yrgb_addr, dst.as_raw_fd() as u64);
        }
        // A short destination is refused before any call.
        let short = buffer(15);
        assert!(channel_job(8, 2, Channel::Red, src.as_fd(), short.as_fd()).is_err());
    }

    #[test]
    fn a_fill_describes_its_rectangle_and_colour() {
        let image = Image::packed(RgaFormat::Rgba8888, 64, 32);
        let buf = buffer(image.frame_len() as u64);
        let job = fill_job(image, buf.as_fd(), (4, 2, 10, 6), [1, 2, 3, 4]).unwrap();
        let req = job.request();
        assert_eq!(req.render_mode, 2);
        assert_eq!(req.fg_color, 0x0403_0201);
        assert_eq!((req.clip.xmax, req.clip.ymax), (63, 31));
        assert_eq!((req.dst().x_offset, req.dst().act_w), (4, 10));
        job.check().unwrap();
        assert!(fill_job(image, buf.as_fd(), (60, 0, 10, 1), [0; 4]).is_err());
        let short = buffer(image.frame_len() as u64 - 1);
        assert!(fill_job(image, short.as_fd(), (0, 0, 1, 1), [0; 4]).is_err());
    }

    #[test]
    fn rops_and_blends_set_their_enable_bits() {
        let (src, dst) = (buffer(16), buffer(16));
        let job = rop_job(8, 2, src.as_fd(), dst.as_fd(), Rop::Xor).unwrap();
        assert_eq!(job.request().alpha_rop_flag, 2);
        assert_eq!(job.request().rop_code, 0xf6);
        assert!(rop_job(6, 2, src.as_fd(), dst.as_fd(), Rop::And).is_err());
        let image = Image::packed(RgaFormat::Rgba8888, 1, 1);
        let job = blend_job(image, src.as_fd(), dst.as_fd(), BlendMode::SrcOver, true).unwrap();
        assert_eq!(job.request().alpha_rop_flag, 0x1 | 0x8);
        assert_eq!(job.request().alpha_rop_mode, 1);
        assert_eq!(job.request().pd_mode, BlendMode::SrcOver.pd_code());
    }

    #[test]
    fn a_copy_is_a_plain_blit_checked_against_both_dma_bufs() {
        let image = Image::packed(RgaFormat::Bgra8888, 4, 2);
        let (src, dst) = (buffer(32), buffer(32));
        let job = copy_job(image, src.as_fd(), dst.as_fd()).unwrap();
        let req = job.request();
        assert_eq!(
            (req.render_mode, req.alpha_rop_flag, req.rotate_mode),
            (0, 0, 0)
        );
        assert_eq!(req.src().yrgb_addr, src.as_raw_fd() as u64);
        assert_eq!(req.dst().yrgb_addr, dst.as_raw_fd() as u64);
        job.check().unwrap();
        let short = buffer(31);
        assert!(copy_job(image, short.as_fd(), dst.as_fd()).is_err());
        assert!(copy_job(image, src.as_fd(), short.as_fd()).is_err());
    }

    #[test]
    fn turns_and_mirrors_pick_the_rotate_mode() {
        let src = Image::packed(RgaFormat::Rgba8888, 4, 2);
        let dst = Image::packed(RgaFormat::Rgba8888, 2, 4);
        let (input, output) = (buffer(32), buffer(32));
        let job =
            transform_job(src, input.as_fd(), dst, output.as_fd(), Transform::Rotate90).unwrap();
        let req = job.request();
        assert_eq!((req.rotate_mode, req.sina, req.cosa), (1, 1 << 16, 0));
        assert!(
            transform_job(src, input.as_fd(), src, output.as_fd(), Transform::Rotate90).is_err()
        );
    }

    #[test]
    fn gray_input_and_colour_keys_carry_their_fields() {
        let (src, dst) = (buffer(64), buffer(64));
        let job = gray_job(4, 2, src.as_fd(), dst.as_fd()).unwrap();
        assert_eq!(job.request().yuv2rgb_mode, 1);
        assert_eq!(job.request().src().format, RgaFormat::Gray8.code() as u32);
        let job = color_key_job(4, 2, src.as_fd(), [1, 2, 3], [4, 5, 6], dst.as_fd()).unwrap();
        assert_eq!(job.request().color_key_min, 0x0003_0201);
        assert_eq!(job.request().color_key_max, 0xff06_0504);
    }
}
