//! A front end job as the kernel module takes it: a pool buffer (output 0), and in order the
//! configuration, the statistics when a statistics block is on, and output 1 when it's on. This
//! is the configuration library's half, so it is `no_std`.
//!
//! [`check`] refuses early what the module will refuse anyway, so a bad configuration is an error
//! for its caller instead of the end of the stream. It is a courtesy, not a defence: the module
//! reads the configuration once into its own memory and proves it there.

use crate::uapi::{FeConfig, RawStatistics, Wire, fe_enable as en, image_format as f};

/// `sizeof(struct pisp_fe_config)`.
pub const CONFIG_BYTES: usize = FeConfig::SIZE;
/// `sizeof(struct pisp_statistics)`, in the block's statistics buffer.
pub const STATS_BYTES: usize = RawStatistics::SIZE;
/// Multiple of which the block's addresses must be.
pub const ALIGN: u64 = 16;

/// The enables of blocks whose results land in the statistics buffer.
const STATS_CLUSTER: u32 = en::STATS_CROP
    | en::DECIMATE
    | en::BLC
    | en::CDAF_STATS
    | en::AWB_STATS
    | en::RGBY
    | en::LSC
    | en::AGC_STATS;
/// Every enable the module knows: the pipeline and the output branches.
const KNOWN: u32 = en::INPUT
    | en::DECOMPRESS
    | en::DECOMPAND
    | en::BLA
    | en::DPC
    | STATS_CLUSTER
    | branch(0)
    | branch(1);

const fn branch(i: usize) -> u32 {
    en::crop(i) | en::downscale(i) | en::compress(i) | en::output(i)
}

/// What the stream fixed: every job must agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fixed {
    /// Input width.
    pub width: u16,
    /// Input height.
    pub height: u16,
    /// Output 0's width.
    pub output_width: u16,
    /// Output 0's height.
    pub output_height: u16,
    /// Output 0's bytes per line.
    pub output_stride: u32,
    /// Output 0's format bits.
    pub output_format: u32,
}

/// Why a job can't be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobError {
    /// An enable the module doesn't know, or the input or output 0 off.
    Enables,
    /// A disabled branch has blocks left on.
    DisabledBranch,
    /// An address field, or `ilines`, set: the module fills those in.
    AddressField,
    /// The input isn't the fixed frame from the receiver.
    Input,
    /// Output 0 isn't what the stream fixed.
    Output0,
    /// Output 1 has no usable format.
    Output1,
    /// A compressed output's format and its compress block disagree.
    Compression,
    /// The decompress block: this path's input is always 16 bits.
    Decompress,
    /// A statistics crop or grid the block could lock up on.
    StatsWindow,
}

/// Which ranges a configuration needs on its job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    /// A statistics block is on, so the job needs the statistics buffer.
    pub stats: bool,
    /// Output 1 is on.
    pub output1: bool,
}

impl Shape {
    /// Ranges on the job: the configuration, then these.
    #[must_use]
    pub const fn count(&self) -> u32 {
        1 + self.stats as u32 + self.output1 as u32
    }
}

/// Whether `format` is a line an output can write: 16 bits, or compressed at 8.
const fn output_format_ok(format: u32) -> bool {
    let mode = format & f::COMPRESSION_MASK;
    let bps = format & f::BPS_MASK;
    format & !(f::BPS_MASK | f::COMPRESSION_MASK) == 0
        && if mode != 0 {
            bps == f::BPS_8
        } else {
            bps == f::BPS_16
        }
}

/// The compress block and the format that sizes its lines.
fn compression_agrees(cfg: &FeConfig, i: usize) -> bool {
    let mode = cfg.ch[i].output.format.format & f::COMPRESSION_MASK;
    let compressing = cfg.global.enables & en::compress(i) != 0;
    compressing == (mode != 0)
        && (!compressing || u32::from(cfg.ch[i].compress.mode) == mode / f::COMPRESSION_MODE_1)
}

/// Crops and grids of the statistics path, as the module holds them.
fn stats_windows_ok(cfg: &FeConfig) -> bool {
    let enables = cfg.global.enables;
    let min = if enables & en::DECIMATE != 0 { 4 } else { 2 };
    let (w, h) = (cfg.input.format.width, cfg.input.format.height);
    let (a, b, c) = (&cfg.agc_stats, &cfg.awb_stats, &cfg.cdaf_stats);
    let crop = if enables & en::STATS_CROP != 0 {
        let s = &cfg.stats_crop;
        s.width >= min
            && s.height >= min
            && s.offset_x < (w & !1)
            && s.offset_y < h
            && u32::from(s.offset_x) + u32::from(s.width) <= u32::from(w)
            && u32::from(s.offset_y) + u32::from(s.height) <= u32::from(h)
    } else {
        w >= min && h >= min
    };
    crop && (enables & en::AGC_STATS == 0
        || (a.size_x >= 2 && a.size_y >= 2 && a.row_size_x >= 2 && a.row_size_y >= 2))
        && (enables & en::AWB_STATS == 0 || (b.size_x >= 2 && b.size_y >= 2))
        && (enables & en::CDAF_STATS == 0 || (c.size_x >= 2 && c.size_y >= 2))
}

/// Bytes output `i` writes: stride times height, `None` for a bad stride or format or no such
/// output.
#[must_use]
pub fn output_bytes(cfg: &FeConfig, i: usize) -> Option<u64> {
    let o = &cfg.ch.get(i)?.output.format;
    if !output_format_ok(o.format) {
        return None;
    }
    let stride = u32::try_from(o.stride).ok()?;
    if stride == 0 || stride & 15 != 0 || o.stride2 != 0 {
        return None;
    }
    u64::from(stride).checked_mul(u64::from(o.height))
}

/// `cfg` against what the module will check: the shape of its job.
pub fn check(cfg: &FeConfig, fixed: &Fixed) -> Result<Shape, JobError> {
    let enables = cfg.global.enables;
    if enables & !KNOWN != 0 || enables & (en::INPUT | en::output(0)) != en::INPUT | en::output(0) {
        return Err(JobError::Enables);
    }
    if enables & en::DECOMPRESS != 0 {
        return Err(JobError::Decompress);
    }
    let output1 = enables & en::output(1) != 0;
    if !output1 && enables & branch(1) != 0 {
        return Err(JobError::DisabledBranch);
    }
    let addressed = cfg.stats_buffer.addr_lo != 0
        || cfg.stats_buffer.addr_hi != 0
        || cfg.input_buffer.addr_lo != 0
        || cfg.input_buffer.addr_hi != 0
        || cfg.input_buffer.frame_id != 0
        || cfg
            .output_buffer
            .iter()
            .any(|b| b.addr_lo != 0 || b.addr_hi != 0)
        || cfg.ch.iter().any(|c| c.output.ilines != 0);
    if addressed {
        return Err(JobError::AddressField);
    }
    let input = &cfg.input.format;
    if cfg.input.streaming != 1
        || input.width != fixed.width
        || input.height != fixed.height
        || input.format != f::BPS_16
        || input.stride != 0
    {
        return Err(JobError::Input);
    }
    let out0 = &cfg.ch[0].output.format;
    if out0.width != fixed.output_width
        || out0.height != fixed.output_height
        || u32::try_from(out0.stride).ok() != Some(fixed.output_stride)
        || out0.format != fixed.output_format
        || output_bytes(cfg, 0).is_none()
    {
        return Err(JobError::Output0);
    }
    if output1 && output_bytes(cfg, 1).is_none() {
        return Err(JobError::Output1);
    }
    if !compression_agrees(cfg, 0) || (output1 && !compression_agrees(cfg, 1)) {
        return Err(JobError::Compression);
    }
    if enables & STATS_CLUSTER != 0 && !stats_windows_ok(cfg) {
        return Err(JobError::StatsWindow);
    }
    Ok(Shape {
        stats: enables & STATS_CLUSTER != 0,
        output1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fe::FrontEnd;
    use crate::format::{compute_stride_align, formats};
    use crate::uapi::{BayerOrder, Branch, ImageFormatConfig};

    const FIXED: Fixed = Fixed {
        width: 640,
        height: 480,
        output_width: 640,
        output_height: 480,
        output_stride: 1280,
        output_format: formats::BAYER16,
    };

    fn built() -> FeConfig {
        let mut fe = FrontEnd::new(640, 480, BayerOrder::Rggb);
        let mut image = ImageFormatConfig {
            format: formats::BAYER16,
            ..Default::default()
        };
        compute_stride_align(&mut image, 64);
        fe.set_output_format(Branch::Zero, image);
        fe.enable(en::output(0), true);
        fe.prepare().unwrap()
    }

    #[test]
    fn what_the_library_builds_is_sent() {
        let shape = check(&built(), &FIXED).unwrap();
        assert_eq!(
            shape,
            Shape {
                stats: false,
                output1: false
            }
        );
        assert_eq!(shape.count(), 1);
    }

    #[test]
    fn enabled_statistics_and_output1_need_their_ranges() {
        let mut cfg = built();
        cfg.global.enables |= en::AWB_STATS | en::output(1);
        cfg.awb_stats.size_x = 2;
        cfg.awb_stats.size_y = 2;
        cfg.ch[1].output.format = cfg.ch[0].output.format;
        let shape = check(&cfg, &FIXED).unwrap();
        assert_eq!(shape.count(), 3);
    }

    #[test]
    fn a_compressed_output_needs_its_block_in_the_formats_mode() {
        let fixed = Fixed {
            output_format: f::COMPRESSION_MODE_2,
            ..FIXED
        };
        let mut cfg = built();
        cfg.ch[0].output.format.format = f::COMPRESSION_MODE_2;
        cfg.global.enables |= en::compress(0);
        assert_eq!(check(&cfg, &fixed), Err(JobError::Compression));
        cfg.ch[0].compress.mode = 1;
        assert_eq!(check(&cfg, &fixed), Err(JobError::Compression));
        cfg.ch[0].compress.mode = 2;
        assert!(check(&cfg, &fixed).is_ok());
    }

    #[test]
    fn refused_before_the_module_has_to() {
        type Mutate = fn(&mut FeConfig);
        let cases: [(Mutate, JobError); 16] = [
            (|c| c.global.enables |= 1 << 13, JobError::Enables),
            (|c| c.global.enables &= !en::INPUT, JobError::Enables),
            (
                |c| c.global.enables |= en::crop(1),
                JobError::DisabledBranch,
            ),
            (|c| c.stats_buffer.addr_lo = 16, JobError::AddressField),
            (|c| c.output_buffer[1].addr_hi = 1, JobError::AddressField),
            (|c| c.ch[0].output.ilines = 1, JobError::AddressField),
            (|c| c.input.format.width = 2, JobError::Input),
            (|c| c.ch[0].output.format.stride = 1296, JobError::Output0),
            (|c| c.ch[0].output.format.format = 0, JobError::Output0),
            (|c| c.global.enables |= en::DECOMPRESS, JobError::Decompress),
            (
                |c| c.global.enables |= en::compress(0),
                JobError::Compression,
            ),
            (
                |c| {
                    c.global.enables |= en::output(1);
                    c.ch[1].output.format = c.ch[0].output.format;
                    c.ch[1].output.format.format |= f::ORDER_SWAPPED;
                },
                JobError::Output1,
            ),
            (
                |c| {
                    c.global.enables |= en::output(1);
                    c.ch[1].output.format = c.ch[0].output.format;
                    c.ch[1].output.format.format = f::BPS_12;
                },
                JobError::Output1,
            ),
            (
                |c| {
                    c.global.enables |= en::AWB_STATS;
                    c.awb_stats.size_x = 0;
                },
                JobError::StatsWindow,
            ),
            (
                |c| {
                    c.global.enables |= en::STATS_CROP | en::DECIMATE;
                    c.stats_crop.width = 2;
                    c.stats_crop.height = 480;
                },
                JobError::StatsWindow,
            ),
            (
                |c| {
                    c.global.enables |= en::AGC_STATS;
                    c.agc_stats.size_x = 2;
                    c.agc_stats.size_y = 2;
                    c.agc_stats.row_size_x = 2;
                    c.agc_stats.row_size_y = 0;
                },
                JobError::StatsWindow,
            ),
        ];
        for (mutate, want) in cases {
            let mut cfg = built();
            mutate(&mut cfg);
            assert_eq!(check(&cfg, &FIXED), Err(want));
        }
    }
}
