//! The checks a parameter buffer passes before the kernel sees it.
//!
//! A parameter buffer is the caller's: a [`Params`] head (how each run of its model is laid on
//! the block) followed by the model's weights. The vendor driver uses its numbers as indices into
//! its own arrays and as register values without checking any of them, and points the block's
//! weight reads at `buffer + kwt_offet[run]` for `kwt_size[run]` bytes. [`check`] refuses every
//! value that would take the kernel or the block outside what it owns. It reads only those
//! fields; the weights and every other number are carried as they are.
//!
//! What each check stands for, from `aiisp.c` (rockchip-linux/kernel `develop-6.1` at
//! `77168c8d5ab82399f65a80e9f807b50ba37cf483`):
//!
//! - `model_runcnt` 1 to the node's `max_runcnt` (at most [`MAX_RUNCNT`]): `model_cfg[run_idx]`
//!   and the `kwt_cfg` arrays are indexed by every run up to it.
//! - `model_mode` one of [`model_mode`]: it picks the output buffers and the layer sizes.
//! - Per run, `sw_aiisp_mode` and `sw_aiisp_op_mode` 0 or 1: they index `channels_lut`,
//!   `mi_chns_tab`, `lst_slice_align_tab` and `ext_tab`.
//! - `sw_aiisp_level_num` 1 to [`LEVELS`]: `sw_aiisp_lv_mode[level_num - 1]` is read.
//! - The last level's `sw_aiisp_lv_mode` below 5: it indexes `channels_lut`'s second dimension.
//!   Every level's `sw_aiisp_lv_mode` below 64 and `sw_aiisp_lv_active` below 4: they are packed
//!   into the level control registers in 6- and 2-bit fields, and a wider value would write the
//!   next level's.
//! - For each enabled read channel, `sw_mi_chn_mode` and `sw_mi_chn_data_mode` below 3: they
//!   index `den_tab`/`num_tab` and `bits_tab`. A channel reading [`chn_src::AIISP_LAST_OUT`]
//!   reads `outbuf_size[model_runcnt - run (+1 in COMBO_MODE)]`, an array of [`MAX_RUNCNT`]:
//!   that index must be inside it.
//! - `sw_aiisp_drop_en`, `sw_mi_chn1_sel` and `sw_mi_chn3_sel` 0 or 1: one-bit register fields
//!   with the next field directly above them.
//! - The weights of each run inside the bytes the caller gave: `kwt_offet + 4 * ceil(kwt_size /
//!   4)` at most the buffer's length. The kernel programs the read as `kwt_size` wide, one line,
//!   with a stride of `ceil(kwt_size / 4)` words, so the read ends there.
//!
//! Not checked here, because it is not in the parameter buffer: how far the block's data channels
//! read and write. That is set by the pool's plane sizes together with each run's channel modes,
//! and is why no operation that reaches the block is offered (see the crate documentation).

use core::mem::{offset_of, size_of};

use crate::uapi::{CHANNELS, KwtCfg, LEVELS, MAX_RUNCNT, ModelCfg, Params, chn_src, model_mode};

/// Why a parameter buffer was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamError {
    /// Shorter than the [`Params`] head, or longer than the kernel's buffer.
    Length {
        /// The bytes given.
        len: usize,
        /// The least allowed.
        min: usize,
        /// The most allowed.
        max: usize,
    },
    /// `model_runcnt` is 0 or more than the node was set up for.
    Runs {
        /// The value given.
        runs: u32,
        /// The most the node takes.
        max: u32,
    },
    /// `model_mode` is not one of [`model_mode`].
    Mode(u32),
    /// A field of run `run`'s [`ModelCfg`] is out of its range.
    Field {
        /// The run.
        run: u32,
        /// The field's name in the header.
        field: &'static str,
        /// The value given.
        value: u32,
    },
    /// Run `run`'s weights end past the buffer, or their end overflows.
    Weights {
        /// The run.
        run: u32,
        /// `kwt_offet[run]`.
        offset: u32,
        /// `kwt_size[run]`.
        size: u32,
    },
    /// A read channel of run `run` reads the previous run's output through an index the kernel's
    /// layer-size table does not have.
    LastOut {
        /// The run.
        run: u32,
        /// The channel.
        channel: u32,
    },
}

/// What [`check`] found in a parameter buffer it took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checked {
    /// `model_runcnt`.
    pub runs: u32,
    /// `model_mode`.
    pub mode: u32,
    /// `module_update`.
    pub update: u32,
    /// `frame_id`.
    pub frame_id: u32,
}

/// The `u32` at `offset` in `bytes`, which the caller has checked holds it.
fn word(bytes: &[u8], offset: usize) -> u32 {
    let mut raw = [0; 4];
    if let Some(src) = bytes.get(offset..offset + 4) {
        raw.copy_from_slice(src);
    }
    u32::from_ne_bytes(raw)
}

const MODEL: usize = offset_of!(Params, model_cfg);
const KWT: usize = offset_of!(Params, kwt_cfg);

/// The `u32` field at `field` (an offset in [`ModelCfg`]), element `index`, of run `run`.
fn model(bytes: &[u8], run: u32, field: usize, index: usize) -> u32 {
    word(
        bytes,
        MODEL + run as usize * size_of::<ModelCfg>() + field + index * 4,
    )
}

/// Checks `bytes`, a parameter buffer, for a node set up with `max_runcnt` runs at most and whose
/// kernel buffers hold `capacity` bytes. Reads nothing past the fields listed in the module
/// documentation and allocates nothing.
pub fn check(bytes: &[u8], max_runcnt: u32, capacity: usize) -> Result<Checked, ParamError> {
    let min = size_of::<Params>();
    if bytes.len() < min || bytes.len() > capacity {
        return Err(ParamError::Length {
            len: bytes.len(),
            min,
            max: capacity,
        });
    }
    let runs = word(bytes, offset_of!(Params, model_runcnt));
    let max = max_runcnt.min(MAX_RUNCNT);
    if runs == 0 || runs > max {
        return Err(ParamError::Runs { runs, max });
    }
    let mode = word(bytes, offset_of!(Params, model_mode));
    if mode > model_mode::AIYNR_MODE {
        return Err(ParamError::Mode(mode));
    }
    for run in 0..runs {
        check_run(bytes, run, runs, mode)?;
        check_weights(bytes, run)?;
    }
    Ok(Checked {
        runs,
        mode,
        update: word(bytes, offset_of!(Params, module_update)),
        frame_id: word(bytes, offset_of!(Params, frame_id)),
    })
}

/// `value` is below `limit`, or the error for `field` of `run`.
fn below(run: u32, field: &'static str, value: u32, limit: u32) -> Result<(), ParamError> {
    if value < limit {
        Ok(())
    } else {
        Err(ParamError::Field { run, field, value })
    }
}

fn check_run(bytes: &[u8], run: u32, runs: u32, mode: u32) -> Result<(), ParamError> {
    let one = |field: usize| model(bytes, run, field, 0);
    below(
        run,
        "sw_aiisp_mode",
        one(offset_of!(ModelCfg, sw_aiisp_mode)),
        2,
    )?;
    below(
        run,
        "sw_aiisp_op_mode",
        one(offset_of!(ModelCfg, sw_aiisp_op_mode)),
        2,
    )?;
    below(
        run,
        "sw_aiisp_drop_en",
        one(offset_of!(ModelCfg, sw_aiisp_drop_en)),
        2,
    )?;
    below(
        run,
        "sw_mi_chn1_sel",
        one(offset_of!(ModelCfg, sw_mi_chn1_sel)),
        2,
    )?;
    below(
        run,
        "sw_mi_chn3_sel",
        one(offset_of!(ModelCfg, sw_mi_chn3_sel)),
        2,
    )?;
    let levels = one(offset_of!(ModelCfg, sw_aiisp_level_num));
    if levels == 0 || levels > LEVELS as u32 {
        return Err(ParamError::Field {
            run,
            field: "sw_aiisp_level_num",
            value: levels,
        });
    }
    for level in 0..LEVELS {
        let lv_mode = model(bytes, run, offset_of!(ModelCfg, sw_aiisp_lv_mode), level);
        below(run, "sw_aiisp_lv_mode", lv_mode, 64)?;
        let active = model(bytes, run, offset_of!(ModelCfg, sw_aiisp_lv_active), level);
        below(run, "sw_aiisp_lv_active", active, 4)?;
    }
    let last = model(
        bytes,
        run,
        offset_of!(ModelCfg, sw_aiisp_lv_mode),
        levels as usize - 1,
    );
    below(run, "sw_aiisp_lv_mode", last, 5)?;
    for channel in 0..CHANNELS {
        if model(bytes, run, offset_of!(ModelCfg, sw_mi_chn_en), channel) == 0 {
            continue;
        }
        let chn_mode = model(bytes, run, offset_of!(ModelCfg, sw_mi_chn_mode), channel);
        below(run, "sw_mi_chn_mode", chn_mode, 3)?;
        let data_mode = model(
            bytes,
            run,
            offset_of!(ModelCfg, sw_mi_chn_data_mode),
            channel,
        );
        below(run, "sw_mi_chn_data_mode", data_mode, 3)?;
        let src = model(bytes, run, offset_of!(ModelCfg, mi_chn_src), channel);
        if src == chn_src::AIISP_LAST_OUT {
            // `outbuf_size[model_runcnt - run_idx (+ 1)]`; `run < runs`, so no wrap.
            let index = runs - run + u32::from(mode == model_mode::COMBO_MODE);
            if index >= MAX_RUNCNT {
                return Err(ParamError::LastOut {
                    run,
                    channel: channel as u32,
                });
            }
        }
    }
    Ok(())
}

fn check_weights(bytes: &[u8], run: u32) -> Result<(), ParamError> {
    let offset = word(
        bytes,
        KWT + offset_of!(KwtCfg, kwt_offet) + run as usize * 4,
    );
    let size = word(bytes, KWT + offset_of!(KwtCfg, kwt_size) + run as usize * 4);
    let end = u64::from(offset) + u64::from(size).div_ceil(4) * 4;
    if end > bytes.len() as u64 {
        return Err(ParamError::Weights { run, offset, size });
    }
    Ok(())
}
