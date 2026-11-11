//! The deinterlacer as `codec.block/1` has it. The block's name is `rk-mpp:iep2`.
//!
//! A job is one pass of the block: its settings, and its frames' planes, each a part of a
//! buffer of the job stream. The kernel's job is a parameter structure whose descriptor words
//! it translates and whose other words it programs the block from, checking nothing; so this
//! driver builds it, from the caller's numbers, and only out of planes checked against the
//! real size of the dma-buf each sits in. The work is:
//!
//! - **Item [`TAG_CONFIG`]**, exactly one, [`CONFIG_BYTES`] long, twelve little-endian
//!   `u32`s: the width and height; the input's sampling (2 for 4:2:2, 3 for 4:2:0), chroma
//!   layout (0 Cb first, 1 Cr first, 3 planar), luma stride and chroma stride in bytes; the
//!   output's sampling, chroma layout (0 or 1) and stride in bytes; the mode
//!   ([`DilMode`] as a number), the field order (0 top first, 1 bottom first)
//!   and the pulldown frames ([`PdComp`] as a number).
//! - **Item [`TAG_TUNING`]**, at most one, [`TUNING_BYTES`] long: the detectors' and filters'
//!   settings as the kernel lays them out, `struct iep2_params` from `md_theta` to the end of
//!   `mtn_tab`, in the machine's byte order ([`Tuning::encode`](crate::Tuning::encode) makes
//!   it). They are carried as they are but for `osd_area_num`, at most 8. Without the item they
//!   are all 0: no OSD areas and the kernel's own motion table.
//! - **A buffer reference for each plane**: its slot is the plane's place in [`Slot::ALL`]
//!   (0 the current frame's luma, up to 14 the motion data), its `offset` where the plane
//!   starts, its `length` 0. Every slot is named once, but the three Cr slots, which are named
//!   for a planar input and only then.
//!
//! The read-back is the block's counts, [`RESULTS_BYTES`] bytes: `struct iep2_output` as the
//! kernel lays it out, each word little-endian.
//!
//! What is checked is listed in [`crate::job`]: the size, each stride, every plane's extent
//! inside the real size of its buffer (asked of the kernel when the claim takes the buffer,
//! overflow-free), the layouts, the modes, the OSD count; then `lemnos-driver-rk-mpp` writes
//! every descriptor word with a buffer the claim holds and checks the offsets and words again.
//! Anything else is refused: another tag, a second item of a tag, an item of another length,
//! a value no layout or mode has, a slot past 14 or named twice, a length that isn't 0, a
//! stream buffer without a handle.
//!
//! # One consumer, one claim
//!
//! The claim on the service is taken when the first job comes and given up when the consumer
//! goes ([`Block::release`]): the kernel then unmaps everything the session mapped. A claim
//! that can't be taken fails the job and is tried again by the next, behind the service's own
//! backoff (doubling to 8 s; it never gives up). A job runs to its end inside
//! [`Block::start`]: the kernel's poll waits for it and bounds the wait itself.

use lemnos_contract_codec_block::work::Work;
use lemnos_contract_codec_block::{
    BUFFER_REF_BYTES, BlockInfo, ITEM_HEADER_BYTES, WORK_HEADER_BYTES,
};
use lemnos_core::Health;
use lemnos_driver_codec_jobs::{Block, Buffers, Start};
use lemnos_driver_rk_mpp::codec::block_name;
use lemnos_driver_rk_mpp::{BlockClaim, Device, MAX_BUFFERS, SubmitError};

use crate::job::{At, Buffer, Chroma, Frame, Input, Job, Mode, Output, SLOTS, Slot};
use crate::params::{
    DilMode, FieldOrder, OSD_AREAS, PdComp, RESULTS_BYTES, Sampling, Swap, TUNING_BYTES,
};

/// The item that carries the job's sizes, layouts and mode.
pub const TAG_CONFIG: u32 = 1;
/// The item that carries the detectors' and filters' settings.
pub const TAG_TUNING: u32 = 2;
/// Bytes of a [`TAG_CONFIG`] item.
pub const CONFIG_BYTES: usize = 12 * 4;
/// The most work a job has: every slot named and both items.
pub const MAX_WORK_BYTES: usize = WORK_HEADER_BYTES
    + SLOTS * BUFFER_REF_BYTES
    + 2 * ITEM_HEADER_BYTES
    + CONFIG_BYTES
    + TUNING_BYTES;

/// Words of the results.
const RESULT_WORDS: usize = RESULTS_BYTES / 4;
const _: () = assert!(OSD_AREAS == 8 && RESULTS_BYTES.is_multiple_of(4));

/// A little-endian `u32` at `at` of `bytes`.
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    let four = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([four[0], four[1], four[2], four[3]]))
}

fn sampling(value: u32) -> Option<Sampling> {
    [Sampling::Yuv422, Sampling::Yuv420]
        .into_iter()
        .find(|&s| s as u32 == value)
}

fn swap(value: u32) -> Option<Swap> {
    [Swap::SemiUv, Swap::SemiVu, Swap::Planar]
        .into_iter()
        .find(|&s| s as u32 == value)
}

fn dil_mode(value: u32) -> Option<DilMode> {
    [
        DilMode::Disable,
        DilMode::I5O2,
        DilMode::I5O1T,
        DilMode::I5O1B,
        DilMode::I2O2,
        DilMode::I1O1B,
        DilMode::Pd,
        DilMode::Bypass,
        DilMode::Dect,
    ]
    .into_iter()
    .find(|&m| m as u32 == value)
}

fn field_order(value: u32) -> Option<FieldOrder> {
    [FieldOrder::TopFirst, FieldOrder::BottomFirst]
        .into_iter()
        .find(|&f| f as u32 == value)
}

fn pd_comp(value: u32) -> Option<PdComp> {
    [PdComp::CurCur, PdComp::CurNext, PdComp::NextCur]
        .into_iter()
        .find(|&p| p as u32 == value)
}

/// What a [`TAG_CONFIG`] item says, before the planes are placed.
#[derive(Clone, Copy)]
struct Config {
    width: u32,
    height: u32,
    in_sampling: Sampling,
    in_swap: Swap,
    luma_stride: u32,
    chroma_stride: u32,
    out_sampling: Sampling,
    out_swap: Swap,
    out_stride: u32,
    mode: Mode,
}

fn config(bytes: &[u8]) -> Option<Config> {
    if bytes.len() != CONFIG_BYTES {
        return None;
    }
    let w = |i: usize| word(bytes, 4 * i);
    Some(Config {
        width: w(0)?,
        height: w(1)?,
        in_sampling: sampling(w(2)?)?,
        in_swap: swap(w(3)?)?,
        luma_stride: w(4)?,
        chroma_stride: w(5)?,
        out_sampling: sampling(w(6)?)?,
        out_swap: swap(w(7)?)?,
        out_stride: w(8)?,
        mode: Mode {
            dil: dil_mode(w(9)?)?,
            field_order: field_order(w(10)?)?,
            pd: pd_comp(w(11)?)?,
        },
    })
}

/// Whether `slot` is a Cr plane, named only for a planar input.
fn is_cr(slot: Slot) -> bool {
    matches!(slot, Slot::CurCr | Slot::NextCr | Slot::PrevCr)
}

/// The deinterlacer as `codec.block/1` has it, on the service's claim.
pub struct IepBlock<D: Device> {
    claim: BlockClaim<D>,
    /// The job's settings, built here: the tuning item, or zeros.
    tuning: [u8; TUNING_BYTES],
    /// The stream buffer and offset each slot names.
    named: [Option<(u32, u32)>; SLOTS],
    results: [u32; RESULT_WORDS],
    identity: u64,
}

impl<D: Device> IepBlock<D> {
    /// The IEP2 of the service on `device`. Nothing is opened until a job comes.
    pub fn new(device: D) -> Self {
        Self {
            claim: BlockClaim::new(device, lemnos_driver_rk_mpp::Block::Iep2),
            tuning: [0; TUNING_BYTES],
            named: [None; SLOTS],
            results: [0; RESULT_WORDS],
            identity: 0,
        }
    }

    /// Whether the block holds its claim.
    pub fn claimed(&self) -> bool {
        self.claim.claimed()
    }

    /// The claim's buffer for the stream buffer `slot` names, at its offset.
    fn at(&mut self, slot: Slot, buffers: &Buffers<'_>) -> Result<At, Start> {
        let index = Slot::ALL.iter().position(|&s| s == slot).unwrap_or(0);
        let Some((buffer, offset)) = self.named[index] else {
            return Err(Start::Refused);
        };
        let id = self.claim.buffer(buffer, buffers)?;
        let len = self
            .claim
            .held()
            .and_then(|claim| claim.buffer_size(id))
            .ok_or(Start::Failed)?;
        Ok(At::new(Buffer { id, len }, u64::from(offset)))
    }

    fn frame(
        &mut self,
        planar: bool,
        [y, c, cr]: [Slot; 3],
        buffers: &Buffers<'_>,
    ) -> Result<Frame, Start> {
        let luma = self.at(y, buffers)?;
        let chroma = if planar {
            Chroma::Planar {
                cb: self.at(c, buffers)?,
                cr: self.at(cr, buffers)?,
            }
        } else {
            Chroma::Semi(self.at(c, buffers)?)
        };
        Ok(Frame { luma, chroma })
    }

    /// Builds and runs the job the work and the named slots describe, on a held claim.
    fn run(&mut self, c: &Config, buffers: &Buffers<'_>, read_back: &mut [u8]) -> Start {
        let planar = matches!(c.in_swap, Swap::Planar);
        let input = Input {
            sampling: c.in_sampling,
            swap: c.in_swap,
            luma_stride: c.luma_stride,
            chroma_stride: c.chroma_stride,
            cur: match self.frame(planar, [Slot::CurY, Slot::CurC, Slot::CurCr], buffers) {
                Ok(frame) => frame,
                Err(start) => return start,
            },
            next: match self.frame(planar, [Slot::NextY, Slot::NextC, Slot::NextCr], buffers) {
                Ok(frame) => frame,
                Err(start) => return start,
            },
            prev: match self.frame(planar, [Slot::PrevY, Slot::PrevC, Slot::PrevCr], buffers) {
                Ok(frame) => frame,
                Err(start) => return start,
            },
        };
        let output = Output {
            sampling: c.out_sampling,
            swap: c.out_swap,
            stride: c.out_stride,
            top: match self.frame(false, [Slot::TopY, Slot::TopC, Slot::TopC], buffers) {
                Ok(frame) => frame,
                Err(start) => return start,
            },
            bottom: match self.frame(
                false,
                [Slot::BottomY, Slot::BottomC, Slot::BottomC],
                buffers,
            ) {
                Ok(frame) => frame,
                Err(start) => return start,
            },
        };
        let (md, mv) = match (self.at(Slot::Md, buffers), self.at(Slot::Mv, buffers)) {
            (Ok(md), Ok(mv)) => (md, mv),
            (Err(start), _) | (_, Err(start)) => return start,
        };
        let Ok(job) = Job::new(
            c.width,
            c.height,
            &input,
            &output,
            md,
            mv,
            c.mode,
            &self.tuning,
        ) else {
            return Start::Refused;
        };

        self.identity = self.identity.wrapping_add(1);
        let Some(claim) = self.claim.held() else {
            return Start::Failed;
        };
        match claim.submit_params(&job.param_job(self.identity)) {
            Ok(()) => {}
            Err(SubmitError::Job(_)) => return Start::Refused,
            Err(_) => {
                // The kernel refused the call, or the session is in a state nobody knows: the
                // next job starts on a new one.
                self.claim.unclaim();
                return Start::Failed;
            }
        }
        if claim.complete(&mut self.results).is_err() {
            self.claim.unclaim();
            return Start::Failed;
        }
        let Some(into) = read_back.get_mut(..RESULTS_BYTES) else {
            return Start::Failed;
        };
        for (four, value) in into.as_chunks_mut::<4>().0.iter_mut().zip(&self.results) {
            *four = value.to_le_bytes();
        }
        Start::Done(RESULTS_BYTES)
    }
}

impl<D: Device> Block for IepBlock<D> {
    fn name(&self) -> &str {
        block_name(self.claim.block())
    }

    fn limits(&self) -> BlockInfo {
        BlockInfo {
            max_work_bytes: MAX_WORK_BYTES as u32,
            max_refs: SLOTS as u16,
            max_items: 2,
            max_read_back: RESULTS_BYTES as u32,
            max_buffers: MAX_BUFFERS as u32,
        }
    }

    fn start(
        &mut self,
        work: &Work<'_>,
        buffers: &Buffers<'_>,
        read_back: &mut [u8],
        now_ns: u64,
    ) -> Start {
        // The work's shape first: nothing is opened for work that is refused anyway.
        let mut config_item = None;
        let mut tuning = None;
        for (tag, bytes) in work.items() {
            match tag {
                TAG_CONFIG if config_item.is_none() => config_item = Some(bytes),
                TAG_TUNING if tuning.is_none() && bytes.len() == TUNING_BYTES => {
                    tuning = Some(bytes);
                }
                _ => return Start::Refused,
            }
        }
        let Some(c) = config_item.and_then(config) else {
            return Start::Refused;
        };
        match tuning {
            Some(bytes) => self.tuning.copy_from_slice(bytes),
            None => self.tuning.fill(0),
        }
        if work.ref_count() > SLOTS {
            return Start::Refused;
        }
        self.named = [None; SLOTS];
        for reference in work.refs() {
            let Some(slot) = self.named.get_mut(reference.slot as usize) else {
                return Start::Refused;
            };
            if slot.is_some() || reference.length != 0 {
                return Start::Refused;
            }
            *slot = Some((reference.buffer, reference.offset));
        }
        // Every slot named, the Cr ones exactly when the input is planar.
        let planar = matches!(c.in_swap, Swap::Planar);
        for (slot, named) in Slot::ALL.iter().zip(&self.named) {
            if named.is_some() != (!is_cr(*slot) || planar) {
                return Start::Refused;
            }
        }
        if self.claim.claim(now_ns).is_none() {
            return Start::Failed;
        }
        self.run(&c, buffers, read_back)
    }

    fn release(&mut self) {
        self.claim.unclaim();
    }

    fn health(&self) -> Health {
        self.claim.health()
    }
}
