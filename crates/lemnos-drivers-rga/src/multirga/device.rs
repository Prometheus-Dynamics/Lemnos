//! [`MultiRga`]: one `rga_req` per operation, sent with `RGA_BLIT_SYNC` on `/dev/rga`. Each
//! operation first builds its request as a [`Job`] (no device touched, so the field mappings are
//! tested on any host), then runs it. Buffers are [`Buffer`]s: a dma-buf, or bytes at their
//! virtual address; the job borrows them until the call returns.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use lemnos_linux_sys::rga::{self, ImgInfo, Job, Rect as RgaRect};

pub use lemnos_linux_sys::rga::Buffer;

use super::{BlendMode, Image, RgaFormat, Rop};
use crate::Transform;

/// The device node.
pub const NODE: &str = "/dev/rga";

/// `render_mode` values (`rga.h`).
const BITBLT: u8 = 0x0;
const COLOR_PALETTE: u8 = 0x1;
const COLOR_FILL: u8 = 0x2;

/// `alpha_rop_flag` bits (`rga.h`).
const ALPHA_ROP_ENABLE: u16 = 0x1;
const ROP_ENABLE: u16 = 0x2;
const PD_ENABLE: u16 = 0x8;

/// `palette_mode` for 8-bit indices (`rga.h`: 3 is "8bpp").
const PALETTE_8BPP: u8 = 3;

/// `yuv2rgb_mode` for BT.601 full range (`rga.h`: 1 is "BT.601 JPEG").
const YUV2RGB_BT601_FULL: u8 = 1;

/// `rotate_mode` values (`rga.h`): 1 rotate by `sina`/`cosa`, 2 x mirror, 3 y mirror.
const ROTATE_BY_ANGLE: u8 = 1;
const X_MIRROR: u8 = 2;
const Y_MIRROR: u8 = 3;

/// 1.0 in the header's 16.16 sine and cosine.
const ONE: i32 = 1 << 16;

/// The multi-RGA node, open for requests. See the module docs and the crate docs for what each
/// operation's route covers and which field mappings are unverified.
#[derive(Debug)]
pub struct MultiRga {
    file: File,
    path: PathBuf,
}

/// The former name of [`MultiRga`]. It named a vendor library this crate no longer loads; the
/// replacement is the kernel's own multi-RGA node.
#[deprecated(
    since = "2.0.1",
    note = "the vendor library route is gone: use `MultiRga` (the `/dev/rga` node)"
)]
pub type Librga = MultiRga;

impl MultiRga {
    /// Opens `/dev/rga` read-write, when it exists and opens.
    pub fn find() -> Option<Self> {
        let file = OpenOptions::new().read(true).write(true).open(NODE).ok()?;
        Some(Self {
            file,
            path: PathBuf::from(NODE),
        })
    }

    /// The device node.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn run(&self, job: &mut Job<'_>) -> io::Result<()> {
        rga::blit_sync(self.file.as_fd(), job)
    }

    /// Fills `rect` (left, top, width, height) of `dst` with `rgba` (a solid colour fill).
    /// Field mapping unverified: the colour's byte order (`0xAABBGGRR` as the crate packs it),
    /// and whether the active rectangle is the fill area.
    pub fn fill(
        &self,
        image: Image,
        dst: Buffer<'_>,
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
        src: Buffer<'_>,
        dst: Buffer<'_>,
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
        fg: Buffer<'_>,
        dst: Buffer<'_>,
        mode: BlendMode,
        premultiplied: bool,
    ) -> io::Result<()> {
        self.run(&mut blend_job(image, fg, dst, mode, premultiplied)?)
    }

    /// 8-bit indices (`width` by `height`) to RGBA8888 through a 256-entry table of
    /// `[r, g, b, a]` entries. Field mapping unverified: the table's entry layout (the header
    /// gives the table's address and the 8-bit depth, not its format).
    pub fn palette<'a>(
        &self,
        width: u32,
        height: u32,
        indices: Buffer<'a>,
        table: &'a [[u8; 4]; 256],
        dst: Buffer<'a>,
    ) -> io::Result<()> {
        self.run(&mut palette_job(width, height, indices, table, dst)?)
    }

    /// The pixels of `src` (R, G, B, X) whose every channel lies in `min..=max` drawn opaque onto
    /// `dst`, which the caller clears first (its alpha is then the mask). Field mapping
    /// unverified: the header has the colour key range but not its enable bit or inversion mode,
    /// so the request sends the range alone.
    pub fn color_key(
        &self,
        width: u32,
        height: u32,
        src: Buffer<'_>,
        min: [u8; 3],
        max: [u8; 3],
        dst: Buffer<'_>,
    ) -> io::Result<()> {
        self.run(&mut color_key_job(width, height, src, min, max, dst)?)
    }

    /// Gray (Y only, full range) to RGBA8888 with alpha opaque, the same size. Field mapping
    /// unverified: the chroma of a Y-only source is taken as neutral.
    pub fn gray_to_rgba(
        &self,
        width: u32,
        height: u32,
        src: Buffer<'_>,
        dst: Buffer<'_>,
    ) -> io::Result<()> {
        self.run(&mut gray_job(width, height, src, dst)?)
    }

    /// Flips or turns `src` into `dst` (the output size of `transform`). Field mapping
    /// unverified: which mirror is horizontal, and the direction of a quarter turn (clockwise is
    /// assumed).
    pub fn transform(
        &self,
        src: Image,
        input: Buffer<'_>,
        dst: Image,
        output: Buffer<'_>,
        transform: Transform,
    ) -> io::Result<()> {
        self.run(&mut transform_job(src, input, dst, output, transform)?)
    }
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
fn fill_job<'a>(
    image: Image,
    dst: Buffer<'a>,
    rect: (u32, u32, u32, u32),
    rgba: [u8; 4],
) -> io::Result<Job<'a>> {
    check_rect(rect, image)?;
    let mut target = info(image)?;
    target.x_offset = dim(rect.0)?;
    target.y_offset = dim(rect.1)?;
    target.act_w = dim(rect.2)?;
    target.act_h = dim(rect.3)?;
    let mut job = Job::new();
    let req = job.req();
    req.render_mode = COLOR_FILL;
    req.color_fill_mode = 0;
    req.fg_color = u32::from_le_bytes(rgba);
    req.clip = full_clip(image)?;
    job.set_dst(target, dst)?;
    Ok(job)
}

/// The request of a mask ROP: masks are RGBA8888 at a quarter of the width.
fn rop_job<'a>(
    width: u32,
    height: u32,
    src: Buffer<'a>,
    dst: Buffer<'a>,
    op: Rop,
) -> io::Result<Job<'a>> {
    if width == 0 || height == 0 || !width.is_multiple_of(4) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("mask {width}x{height}: the width must be a non-zero multiple of four"),
        ));
    }
    let mask = Image::packed(RgaFormat::Rgba8888, width / 4, height);
    let mut job = Job::new();
    let req = job.req();
    req.render_mode = BITBLT;
    req.alpha_rop_flag = ROP_ENABLE;
    req.rop_code = op.code() as u16;
    req.clip = full_clip(mask)?;
    job.set_src(info(mask)?, src)?;
    job.set_dst(info(mask)?, dst)?;
    Ok(job)
}

/// The request of a Porter-Duff blend of `fg` onto `dst`.
fn blend_job<'a>(
    image: Image,
    fg: Buffer<'a>,
    dst: Buffer<'a>,
    mode: BlendMode,
    premultiplied: bool,
) -> io::Result<Job<'a>> {
    let mut job = Job::new();
    let req = job.req();
    req.render_mode = BITBLT;
    req.alpha_rop_flag = ALPHA_ROP_ENABLE | PD_ENABLE;
    req.pd_mode = mode.pd_code();
    req.alpha_rop_mode = u8::from(premultiplied);
    req.alpha_global_value = 0xff;
    req.clip = full_clip(image)?;
    job.set_src(info(image)?, fg)?;
    job.set_dst(info(image)?, dst)?;
    Ok(job)
}

/// The request of an 8-bit palette expansion.
fn palette_job<'a>(
    width: u32,
    height: u32,
    indices: Buffer<'a>,
    table: &'a [[u8; 4]; 256],
    dst: Buffer<'a>,
) -> io::Result<Job<'a>> {
    let src = Image::packed(RgaFormat::Bpp8, width, height);
    let out = Image::packed(RgaFormat::Rgba8888, width, height);
    let mut job = Job::new();
    let req = job.req();
    req.render_mode = COLOR_PALETTE;
    req.palette_mode = PALETTE_8BPP;
    req.clip = full_clip(out)?;
    job.set_src(info(src)?, indices)?;
    job.set_lut(table.as_flattened())?;
    job.set_dst(info(out)?, dst)?;
    Ok(job)
}

/// The request of a colour-key range onto RGBA.
fn color_key_job<'a>(
    width: u32,
    height: u32,
    src: Buffer<'a>,
    min: [u8; 3],
    max: [u8; 3],
    dst: Buffer<'a>,
) -> io::Result<Job<'a>> {
    let input = Image::packed(RgaFormat::Rgbx8888, width, height);
    let out = Image::packed(RgaFormat::Rgba8888, width, height);
    let mut job = Job::new();
    let req = job.req();
    req.render_mode = BITBLT;
    req.color_key_min = u32::from_le_bytes([min[0], min[1], min[2], 0]);
    req.color_key_max = u32::from_le_bytes([max[0], max[1], max[2], 0xff]);
    req.clip = full_clip(out)?;
    job.set_src(info(input)?, src)?;
    job.set_dst(info(out)?, dst)?;
    Ok(job)
}

/// The request of a gray to RGBA conversion.
fn gray_job<'a>(width: u32, height: u32, src: Buffer<'a>, dst: Buffer<'a>) -> io::Result<Job<'a>> {
    let input = Image::packed(RgaFormat::Gray8, width, height);
    let out = Image::packed(RgaFormat::Rgba8888, width, height);
    let mut job = Job::new();
    let req = job.req();
    req.render_mode = BITBLT;
    req.yuv2rgb_mode = YUV2RGB_BT601_FULL;
    req.clip = full_clip(out)?;
    job.set_src(info(input)?, src)?;
    job.set_dst(info(out)?, dst)?;
    Ok(job)
}

/// The request of a flip or quarter turn: the rotate mode and, for turns, `sina`/`cosa`.
fn transform_job<'a>(
    src: Image,
    input: Buffer<'a>,
    dst: Image,
    output: Buffer<'a>,
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
    let (mode, sina, cosa) = match transform {
        Transform::FlipHorizontal => (X_MIRROR, 0, 0),
        Transform::FlipVertical => (Y_MIRROR, 0, 0),
        Transform::Rotate90 => (ROTATE_BY_ANGLE, ONE, 0),
        Transform::Rotate180 => (ROTATE_BY_ANGLE, 0, -ONE),
        Transform::Rotate270 => (ROTATE_BY_ANGLE, -ONE, 0),
    };
    let mut job = Job::new();
    let req = job.req();
    req.render_mode = BITBLT;
    req.rotate_mode = mode;
    req.sina = sina;
    req.cosa = cosa;
    req.clip = full_clip(dst)?;
    job.set_src(info(src)?, input)?;
    job.set_dst(info(dst)?, output)?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fill_describes_its_rectangle_and_colour() {
        let image = Image::packed(RgaFormat::Rgba8888, 64, 32);
        let mut buf = vec![0u8; image.frame_len()];
        let job = fill_job(image, Buffer::Write(&mut buf), (4, 2, 10, 6), [1, 2, 3, 4]).unwrap();
        let req = job.request();
        assert_eq!(req.render_mode, COLOR_FILL);
        assert_eq!(req.fg_color, 0x0403_0201);
        assert_eq!(req.clip.xmax, 63);
        assert_eq!(req.clip.ymax, 31);
        assert!(fill_job(image, Buffer::Write(&mut buf), (60, 0, 10, 1), [0; 4]).is_err());
    }

    #[test]
    fn rops_and_blends_set_their_enable_bits() {
        let mut dst = vec![0u8; 16];
        let src = vec![0u8; 16];
        let job = rop_job(8, 2, Buffer::Read(&src), Buffer::Write(&mut dst), Rop::Xor).unwrap();
        assert_eq!(job.request().alpha_rop_flag, ROP_ENABLE);
        assert_eq!(job.request().rop_code, 0xf6);
        assert!(rop_job(6, 2, Buffer::Read(&src), Buffer::Write(&mut dst), Rop::And).is_err());
        let image = Image::packed(RgaFormat::Rgba8888, 1, 1);
        let (fg, mut bg) = ([0u8; 4], [0u8; 4]);
        let job = blend_job(
            image,
            Buffer::Read(&fg),
            Buffer::Write(&mut bg),
            BlendMode::SrcOver,
            true,
        )
        .unwrap();
        assert_eq!(job.request().alpha_rop_flag, ALPHA_ROP_ENABLE | PD_ENABLE);
        assert_eq!(job.request().alpha_rop_mode, 1);
        assert_eq!(job.request().pd_mode, BlendMode::SrcOver.pd_code());
    }

    #[test]
    fn turns_and_mirrors_pick_the_rotate_mode() {
        let src = Image::packed(RgaFormat::Rgba8888, 4, 2);
        let dst = Image::packed(RgaFormat::Rgba8888, 2, 4);
        let (input, mut output) = (vec![0u8; 32], vec![0u8; 32]);
        let job = transform_job(
            src,
            Buffer::Read(&input),
            dst,
            Buffer::Write(&mut output),
            Transform::Rotate90,
        )
        .unwrap();
        assert_eq!(
            (
                job.request().rotate_mode,
                job.request().sina,
                job.request().cosa
            ),
            (1, ONE, 0)
        );
        assert!(
            transform_job(
                src,
                Buffer::Read(&input),
                src,
                Buffer::Write(&mut output),
                Transform::Rotate90
            )
            .is_err()
        );
    }

    #[test]
    fn a_palette_names_the_eight_bit_depth_and_the_table() {
        let indices = [0u8; 4];
        let table = [[0u8; 4]; 256];
        let mut out = [0u8; 16];
        let job = palette_job(
            2,
            2,
            Buffer::Read(&indices),
            &table,
            Buffer::Write(&mut out),
        )
        .unwrap();
        assert_eq!(job.request().render_mode, COLOR_PALETTE);
        assert_eq!(job.request().palette_mode, 3);
    }
}
