//! A back end job as the kernel module takes it: one configuration slot, one pool buffer, and the
//! device address of each image. This is the configuration library's half, so it is `no_std`.
//!
//! [`check`] refuses early what the module will refuse anyway, so a bad configuration is an error
//! for its caller instead of the end of the stream. It is a courtesy, not a defence: the module
//! proves every address itself, from its own copy.

use crate::uapi::{BE_NUM_OUTPUTS, BE_NUM_TILES, BeConfig, BeTilesConfig, ImageFormatConfig};
use crate::uapi::{bayer_enable, image_format as f, rgb_enable};

/// Addresses a job carries (`LEMNOS_PISP_BE_ADDRS`).
pub const JOB_ADDRS: usize = 13;
/// `sizeof(struct lemnos_pisp_be_job)`.
pub const JOB_BYTES: usize = 120;
/// Bytes of a configuration before the registers the module writes; zero or refused.
const HEAD_BYTES: usize = 112;

/// Slot of the TDN input.
pub const ADDR_TDN_INPUT: usize = 3;
/// Slot of the stitch input.
pub const ADDR_STITCH_INPUT: usize = 4;
/// Slot of the TDN output.
pub const ADDR_TDN_OUTPUT: usize = 5;
/// Slot of the stitch output.
pub const ADDR_STITCH_OUTPUT: usize = 6;

/// Slot of plane `plane` of output `output`.
#[must_use]
pub const fn addr_output(output: usize, plane: usize) -> usize {
    7 + 3 * output + plane
}

/// Why a job can't be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobError {
    /// Bytes the module refuses non-zero: the former address fields, the reserved tail, or AXI.
    ReservedBytes,
    /// No tiles, or more than the block takes.
    TileCount,
    /// HOG can't be sized, so it is never sent.
    Hog,
    /// TDN must either read an average or reset it, and not both.
    TdnInputOrReset,
    /// The configuration enables a stage with no memory given for it.
    MissingMemory,
    /// Memory given for a stage the configuration leaves off.
    UnusedMemory,
    /// An address computation overflowed, or a stride is negative.
    Address,
}

/// Where each image of a job is, as device addresses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JobMemory {
    /// The first plane of the input.
    pub input: u64,
    /// The temporal denoise average read.
    pub tdn_input: Option<u64>,
    /// The temporal denoise average written.
    pub tdn_output: Option<u64>,
    /// The second exposure.
    pub stitch_input: Option<u64>,
    /// The exposure saved for the next frame.
    pub stitch_output: Option<u64>,
    /// The first plane of each output.
    pub output: [Option<u64>; BE_NUM_OUTPUTS],
}

/// What goes on the ring (`struct lemnos_pisp_be_job`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KernelJob {
    /// Carried to the frame.
    pub identity: u64,
    /// The pool buffer the frame fills.
    pub buffer: u32,
    /// The configuration slot.
    pub config: u32,
    /// Device addresses by slot; zero where the job has no such image.
    pub addr: [u64; JOB_ADDRS],
}

/// Refuses what the module would, before anything is posted.
pub fn check(t: &BeTilesConfig) -> Result<(), JobError> {
    let c = &t.config;
    if c.pad0.iter().take(HEAD_BYTES).any(|&b| b != 0)
        || c.pad1.iter().any(|&b| b != 0)
        || (c.axi.r_qos | c.axi.r_cache_prot | c.axi.w_qos | c.axi.w_cache_prot) != 0
    {
        return Err(JobError::ReservedBytes);
    }
    if t.num_tiles == 0 || t.num_tiles as usize > BE_NUM_TILES {
        return Err(JobError::TileCount);
    }
    if c.global.rgb_enables & rgb_enable::HOG != 0 {
        return Err(JobError::Hog);
    }
    let bayer = c.global.bayer_enables;
    let input = bayer & bayer_enable::TDN_INPUT != 0;
    if bayer & bayer_enable::TDN != 0 && input == (c.tdn.reset & 1 != 0) {
        return Err(JobError::TdnInputOrReset);
    }
    Ok(())
}

fn chroma_lines(format: u32, height: u16) -> u64 {
    if format & f::SAMPLING_MASK == f::SAMPLING_420 {
        u64::from(height).div_ceil(2)
    } else {
        u64::from(height)
    }
}

fn planes(format: u32) -> usize {
    if format & f::THREE_CHANNEL == 0 {
        return 1;
    }
    match format & f::PLANARITY_MASK {
        f::PLANARITY_SEMI_PLANAR => 2,
        f::PLANARITY_PLANAR => 3,
        _ => 1,
    }
}

/// The address of each plane, written to `out` from `slot`.
fn place(
    out: &mut [u64; JOB_ADDRS],
    slot: usize,
    image: &ImageFormatConfig,
    base: u64,
) -> Result<(), JobError> {
    let stride = u64::try_from(image.stride).map_err(|_| JobError::Address)?;
    let stride2 = u64::try_from(image.stride2).map_err(|_| JobError::Address)?;
    let mut at = base;
    for plane in 0..planes(image.format) {
        *out.get_mut(slot + plane).ok_or(JobError::Address)? = at;
        let bytes = if plane == 0 {
            stride.checked_mul(u64::from(image.height))
        } else {
            stride2.checked_mul(chroma_lines(image.format, image.height))
        }
        .ok_or(JobError::Address)?;
        at = at.checked_add(bytes).ok_or(JobError::Address)?;
    }
    Ok(())
}

/// One stage: on with memory, or off without it.
fn stage(
    out: &mut [u64; JOB_ADDRS],
    enabled: bool,
    memory: Option<u64>,
    slot: usize,
    image: &ImageFormatConfig,
) -> Result<(), JobError> {
    match (enabled, memory) {
        (true, Some(base)) => place(out, slot, image, base),
        (false, None) => Ok(()),
        (true, None) => Err(JobError::MissingMemory),
        (false, Some(_)) => Err(JobError::UnusedMemory),
    }
}

/// Bytes an image takes in memory: every plane, at its stride.
#[must_use]
pub fn image_bytes(image: &ImageFormatConfig) -> Option<u64> {
    let stride = u64::try_from(image.stride).ok()?;
    let stride2 = u64::try_from(image.stride2).ok()?;
    let luma = stride.checked_mul(u64::from(image.height))?;
    let chroma = stride2.checked_mul(chroma_lines(image.format, image.height))?;
    match planes(image.format) {
        1 => Some(luma),
        2 => luma.checked_add(chroma),
        _ => luma.checked_add(chroma.checked_mul(2)?),
    }
}

/// The job's thirteen addresses, from its configuration and where its images are.
pub fn addresses(c: &BeConfig, memory: &JobMemory) -> Result<[u64; JOB_ADDRS], JobError> {
    let mut out = [0; JOB_ADDRS];
    let bayer = c.global.bayer_enables;
    let rgb = c.global.rgb_enables;
    place(&mut out, 0, &c.input_format, memory.input)?;
    stage(
        &mut out,
        bayer & bayer_enable::TDN_INPUT != 0,
        memory.tdn_input,
        ADDR_TDN_INPUT,
        &c.tdn_input_format,
    )?;
    stage(
        &mut out,
        bayer & bayer_enable::STITCH_INPUT != 0,
        memory.stitch_input,
        ADDR_STITCH_INPUT,
        &c.stitch_input_format,
    )?;
    stage(
        &mut out,
        bayer & bayer_enable::TDN_OUTPUT != 0,
        memory.tdn_output,
        ADDR_TDN_OUTPUT,
        &c.tdn_output_format,
    )?;
    stage(
        &mut out,
        bayer & bayer_enable::STITCH_OUTPUT != 0,
        memory.stitch_output,
        ADDR_STITCH_OUTPUT,
        &c.stitch_output_format,
    )?;
    for (i, base) in memory.output.iter().enumerate() {
        stage(
            &mut out,
            rgb & rgb_enable::output(i) != 0,
            *base,
            addr_output(i, 0),
            &c.output_format.get(i).ok_or(JobError::Address)?.image,
        )?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::be::BackEnd;
    use crate::format::{compute_stride_align, formats};
    use crate::uapi::{BayerOrder, BeCcmConfig};

    fn bayer() -> ImageFormatConfig {
        let mut f = ImageFormatConfig {
            width: 1280,
            height: 800,
            format: formats::BAYER16,
            ..Default::default()
        };
        compute_stride_align(&mut f, 64);
        f
    }

    const CSC: BeCcmConfig = BeCcmConfig {
        coeffs: [306, 601, 117, -173, -339, 512, 512, -429, -83],
        pad: [0; 2],
        offsets: [0, 33554432, 33554432],
    };

    fn nv12() -> BeTilesConfig {
        BackEnd::simple_bayer(
            bayer(),
            BayerOrder::Bggr,
            4096,
            (1.5, 1.0, 1.8),
            None,
            formats::NV12,
            Some(CSC),
        )
        .prepare()
        .unwrap()
    }

    fn memory() -> JobMemory {
        JobMemory {
            input: 0x1000_0000,
            output: [Some(0x2000_0000), None],
            ..Default::default()
        }
    }

    #[test]
    fn a_library_configuration_passes_and_its_planes_are_placed() {
        let t = nv12();
        assert_eq!(check(&t), Ok(()));
        let a = addresses(&t.config, &memory()).unwrap();
        assert_eq!(a[0], 0x1000_0000);
        assert_eq!(a[1], 0);
        assert_eq!(a[addr_output(0, 0)], 0x2000_0000);
        assert_eq!(a[addr_output(0, 1)], 0x2000_0000 + 1280 * 800);
        assert_eq!(a[addr_output(0, 2)], 0);
        assert_eq!(a[addr_output(1, 0)], 0);
    }

    #[test]
    fn the_second_chroma_plane_follows_the_first() {
        let mut t = nv12();
        let o = &mut t.config.output_format[0].image;
        o.format = formats::YUV420P;
        o.stride = 1280;
        o.stride2 = 640;
        let a = addresses(&t.config, &memory()).unwrap();
        assert_eq!(a[addr_output(0, 1)], 0x2000_0000 + 1280 * 800);
        assert_eq!(a[addr_output(0, 2)], 0x2000_0000 + 1280 * 800 + 640 * 400);
    }

    #[test]
    fn what_the_module_refuses_is_refused_here() {
        let mut t = nv12();
        t.config.pad0[0] = 1;
        assert_eq!(check(&t), Err(JobError::ReservedBytes));

        let mut t = nv12();
        t.config.pad1[83] = 1;
        assert_eq!(check(&t), Err(JobError::ReservedBytes));

        let mut t = nv12();
        t.config.axi.w_cache_prot = 1;
        assert_eq!(check(&t), Err(JobError::ReservedBytes));

        let mut t = nv12();
        t.num_tiles = 0;
        assert_eq!(check(&t), Err(JobError::TileCount));
        t.num_tiles = BE_NUM_TILES as u32 + 1;
        assert_eq!(check(&t), Err(JobError::TileCount));

        let mut t = nv12();
        t.config.global.rgb_enables |= rgb_enable::HOG;
        assert_eq!(check(&t), Err(JobError::Hog));
    }

    #[test]
    fn tdn_reads_an_average_or_resets_it_and_not_both_or_neither() {
        let mut t = nv12();
        t.config.global.bayer_enables |= bayer_enable::TDN;
        assert_eq!(check(&t), Err(JobError::TdnInputOrReset));
        t.config.tdn.reset = 1;
        assert_eq!(check(&t), Ok(()));
        t.config.global.bayer_enables |= bayer_enable::TDN_INPUT;
        assert_eq!(check(&t), Err(JobError::TdnInputOrReset));
        t.config.tdn.reset = 0;
        assert_eq!(check(&t), Ok(()));
    }

    #[test]
    fn memory_and_enables_must_agree() {
        let t = nv12();
        let mut m = memory();
        m.tdn_input = Some(0x3000_0000);
        assert_eq!(addresses(&t.config, &m), Err(JobError::UnusedMemory));

        let mut m = memory();
        m.output[0] = None;
        assert_eq!(addresses(&t.config, &m), Err(JobError::MissingMemory));

        let mut m = memory();
        m.output[1] = Some(0x3000_0000);
        assert_eq!(addresses(&t.config, &m), Err(JobError::UnusedMemory));
    }

    #[test]
    fn addresses_that_wrap_or_a_negative_stride_are_refused() {
        let t = nv12();
        let mut m = memory();
        m.output[0] = Some(u64::MAX - 16);
        assert_eq!(addresses(&t.config, &m), Err(JobError::Address));

        let mut t = nv12();
        t.config.output_format[0].image.stride = -1280;
        assert_eq!(addresses(&t.config, &memory()), Err(JobError::Address));
    }

    #[test]
    fn output_slots_are_the_modules() {
        assert_eq!(addr_output(0, 0), 7);
        assert_eq!(addr_output(0, 2), 9);
        assert_eq!(addr_output(1, 0), 10);
        assert_eq!(addr_output(1, 2), 12);
    }
}
