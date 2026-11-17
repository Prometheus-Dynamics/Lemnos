//! The decoder's reach, probed with crafted register files (RKJPEG-DEC).
//!
//! Three questions the decoder's job check needs and no public source answers:
//!
//! 1. **Output format and chroma placement.** Which `yuv_out_format` codes the block completes,
//!    and where it writes the chroma plane relative to the luma plane at `reg13_dec_out_base`
//!    ([`probe_formats`], [`probe_chroma`]).
//! 2. **Table units.** What the three table-length fields count in: `qtbl_len` (`reg7` bits 4:0),
//!    `htbl_mincode_len` (bits 12:8) and `htbl_value_len` (bits 21:16). The probe sweeps the three
//!    together over the values each field can hold and records which values the block completes
//!    with ([`probe_joint`]). Each unit is one candidate per field, and [`match_units`] lists the
//!    candidates that predict the observed set.
//! 3. **Reach.** The smallest dma-buf the block completes a job in, at the chosen format and table
//!    encoding ([`probe_reach`]).
//!
//! Nothing here assumes an answer. A board answer that matches no candidate is reported as such.
//!
//! # Provenance
//!
//! Register indices and field positions are from rockchip-linux/mpp at `9b56348`,
//! `mpp/hal/vpu/jpegd/hal_jpegd_rkv_reg.h` (Apache-2.0), read through a summary of that file. The
//! board run checks each one against the raw header before trusting a result (see
//! `testing/board-runs/RKJPEG-DEC.md`). The register-file size and status bits are this crate's
//! [`crate::regs`], from rockchip-linux/kernel `develop-6.1`. The candidate layouts are hypotheses
//! for the probe to separate, not facts.
//!
//! # Runners
//!
//! [`Runner`] submits one decoder job: a whole register file, and the dma-buf `reg13` names with
//! the bytes the probe wants in it. [`Board`] is the runner on a service node: it goes through
//! `rk-mpp`'s claim and job API, and it is the only runner that reaches hardware. The PC tests run
//! the probe against stand-ins.

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::time::Instant;

use lemnos_driver_rk_mpp::{Address, Block, Device, Job, ReadBack, Service};
use lemnos_linux_sys::mmap::Mapping;

use crate::regs::dec;
use crate::status::{self, Outcome};

/// `reg2_sys`: `yuv_out_format` is bits 29:27.
pub const REG_SYS: usize = 2;
/// The shift of `yuv_out_format` in [`REG_SYS`].
pub const YUV_FMT_SHIFT: u32 = 27;
/// The width of `yuv_out_format`.
pub const YUV_FMT_MASK: u32 = 0b111;
/// `reg3_pic_size`: width minus one in bits 15:0, height minus one in bits 31:16.
pub const REG_PIC: usize = 3;
/// `reg6_y_virstride`: `y_virstride` is bits 31:4.
pub const REG_VSTRIDE: usize = 6;
/// The shift of `y_virstride` in [`REG_VSTRIDE`].
pub const VSTRIDE_SHIFT: u32 = 4;
/// `reg7_tbl_len`: the three table-length fields.
pub const REG_TBL: usize = 7;
/// `reg8_strm_len`: `stream_len` is bits 31:4.
pub const REG_STRM_LEN: usize = 8;
/// `reg9_qtbl_base`.
pub const REG_QTBL: usize = 9;
/// `reg10_htbl_mincode_base`.
pub const REG_HMIN: usize = 10;
/// `reg11_htbl_value_base`.
pub const REG_HVAL: usize = 11;
/// `reg12_strm_base`.
pub const REG_STRM: usize = 12;
/// `reg13_dec_out_base`.
pub const REG_OUT: usize = 13;

/// One of the three table-length fields in [`REG_TBL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// Its shift.
    pub shift: u32,
    /// Its width mask, before the shift.
    pub mask: u32,
}

/// `qtbl_len`, bits 4:0.
pub const QTBL: Field = Field {
    shift: 0,
    mask: 0x1f,
};
/// `htbl_mincode_len`, bits 12:8.
pub const HMIN: Field = Field {
    shift: 8,
    mask: 0x1f,
};
/// `htbl_value_len`, bits 21:16.
pub const HVAL: Field = Field {
    shift: 16,
    mask: 0x3f,
};

/// The three fields in the order the probe uses: quantisation, Huffman mincode, Huffman value.
pub const FIELDS: [Field; 3] = [QTBL, HMIN, HVAL];

/// Where the probe's input dma-buf holds each table and the stream, in bytes from its start. The
/// input buffer is the stream's fixture: `[qtable | mincode | value | stream]`.
pub const QTBL_OFF: u32 = 0;
/// The mincode table's offset in the input buffer.
pub const HMIN_OFF: u32 = 1024;
/// The value table's offset in the input buffer.
pub const HVAL_OFF: u32 = 2048;
/// The stream's offset in the input buffer.
pub const STREAM_OFF: u32 = 4096;

/// The dma-buf's fill byte before a job. The block writes none of it in the stand-ins.
pub const SENTINEL: u8 = 0xa5;
/// What the stand-ins write to the luma plane.
pub const LUMA_FILL: u8 = 0x10;
/// What the stand-ins write to the chroma plane.
pub const CHROMA_FILL: u8 = 0x80;

/// The most bytes the probe's output dma-buf holds.
pub const MEM_BYTES: usize = 1 << 16;

/// The probe's picture: 64 wide, 50 high, luma stride 80 bytes, so the luma plane is 4000 bytes.
pub const WIDTH: u32 = 64;
/// See [`WIDTH`].
pub const HEIGHT: u32 = 50;
/// See [`WIDTH`].
pub const VSTRIDE_BYTES: u32 = 80;
/// The luma plane's length for the probe picture.
pub const LUMA_BYTES: usize = (VSTRIDE_BYTES * HEIGHT) as usize;

/// Submits one decoder job.
pub trait Runner {
    /// Runs `file` (the whole register file, `dec::REG_NUM_V1` words) with `mem` as the output
    /// dma-buf, which `reg13` addresses from byte 0. The bytes of `mem` are the buffer's contents
    /// before the job and are replaced by its contents after. Returns the status word read back.
    fn run(&mut self, file: &[u32], mem: &mut [u8]) -> io::Result<u32>;
}

/// The bytes a table-length field value stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableUnit {
    /// The value is a byte count.
    Bytes,
    /// The value counts 4-byte words.
    Words4,
    /// The value counts 16-byte blocks.
    Blocks16,
    /// The value counts 4-byte words, less one.
    WordsMinusOne,
}

impl TableUnit {
    /// Every candidate.
    pub const ALL: [Self; 4] = [
        Self::Bytes,
        Self::Words4,
        Self::Blocks16,
        Self::WordsMinusOne,
    ];

    /// The bytes a field value `k` makes the block read.
    #[must_use]
    pub const fn consumed(self, k: u32) -> u32 {
        match self {
            Self::Bytes => k,
            Self::Words4 => k * 4,
            Self::Blocks16 => k * 16,
            Self::WordsMinusOne => (k + 1) * 4,
        }
    }

    /// The field value that makes `bytes` bytes, if one fits in `field`.
    #[must_use]
    pub fn encode(self, bytes: u32, field: Field) -> Option<u32> {
        (0..=field.mask).find(|&k| self.consumed(k) == bytes)
    }
}

/// What the fixture's tables hold, in bytes. [`probe_joint`] completes only when each field makes
/// its table's length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Targets {
    /// The quantisation table's length.
    pub qtbl: u32,
    /// The mincode table's length.
    pub mincode: u32,
    /// The value table's length.
    pub value: u32,
}

/// One unit per table-length field.
pub type Units = [TableUnit; 3];

/// The field values in `0..=31` a block completes with, for `units` and `targets`: the value `k`
/// is put in all three fields, and it completes when each field makes its table's length.
#[must_use]
pub fn expected_joint(units: Units, targets: Targets) -> Vec<u32> {
    let wants = [targets.qtbl, targets.mincode, targets.value];
    (0..=QTBL.mask)
        .filter(|&k| {
            units
                .iter()
                .zip(wants)
                .all(|(unit, want)| unit.consumed(k) == want)
        })
        .collect()
}

/// Every unit triple whose [`expected_joint`] set equals `observed`. One match is a unique answer;
/// more is ambiguous; none is an answer the candidates do not predict.
#[must_use]
pub fn match_units(observed: &[u32], targets: Targets) -> Vec<Units> {
    let mut found = Vec::new();
    for a in TableUnit::ALL {
        for b in TableUnit::ALL {
            for c in TableUnit::ALL {
                let units = [a, b, c];
                if expected_joint(units, targets) == observed {
                    found.push(units);
                }
            }
        }
    }
    found
}

/// The chroma plane a candidate places, given the luma plane's bytes. 4:2:0 is assumed: half the
/// luma.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromaAt {
    /// Right after the luma plane, rounded up to 64 bytes.
    AlignedAfterLuma64,
    /// Right after the luma plane.
    Adjacent,
    /// Rounded up to a 4096-byte page.
    Aligned4096,
}

impl ChromaAt {
    /// Every candidate.
    pub const ALL: [Self; 3] = [Self::AlignedAfterLuma64, Self::Adjacent, Self::Aligned4096];

    /// `(start, len)` of the chroma plane for a luma plane of `luma` bytes.
    #[must_use]
    pub fn place(self, luma: usize) -> (usize, usize) {
        let start = match self {
            Self::AlignedAfterLuma64 => luma.next_multiple_of(64),
            Self::Adjacent => luma,
            Self::Aligned4096 => luma.next_multiple_of(4096),
        };
        (start, luma / 2)
    }
}

/// The register file for one job on the probe picture: output at `out`, the format code, and the
/// three table-length fields as `encoding`.
#[must_use]
pub fn file(out: u32, fmt: u32, encoding: [u32; 3]) -> Vec<u32> {
    let mut f = vec![0u32; dec::REG_NUM_V1 as usize];
    f[REG_SYS] = (fmt & YUV_FMT_MASK) << YUV_FMT_SHIFT;
    f[REG_PIC] = (WIDTH - 1) | ((HEIGHT - 1) << 16);
    f[REG_VSTRIDE] = VSTRIDE_BYTES << VSTRIDE_SHIFT;
    f[REG_TBL] = FIELDS
        .iter()
        .zip(encoding)
        .fold(0, |acc, (field, k)| acc | ((k & field.mask) << field.shift));
    f[REG_OUT] = out;
    f
}

/// Which bytes of a dma-buf the block wrote: those no longer the sentinel.
#[must_use]
pub fn written(mem: &[u8]) -> Vec<bool> {
    mem.iter().map(|&b| b != SENTINEL).collect()
}

/// Runs one job on a sentinel-filled output of `len` bytes. Returns the outcome and the written
/// mask.
pub fn run_on<R: Runner>(
    r: &mut R,
    len: usize,
    fmt: u32,
    encoding: [u32; 3],
) -> io::Result<(Outcome, Vec<bool>)> {
    let mut mem = vec![SENTINEL; len];
    let status = r.run(&file(0, fmt, encoding), &mut mem)?;
    Ok((status::decoder_v1(status), written(&mem)))
}

/// Sweeps the 3-bit output format over every code, with `encoding`. Each entry is the format,
/// its outcome, and the bytes it wrote.
pub fn probe_formats<R: Runner>(
    r: &mut R,
    encoding: [u32; 3],
) -> io::Result<Vec<(u32, Outcome, usize)>> {
    let mut rows = Vec::new();
    for fmt in 0..=YUV_FMT_MASK {
        let (outcome, mask) = run_on(r, MEM_BYTES, fmt, encoding)?;
        rows.push((fmt, outcome, mask.iter().filter(|&&w| w).count()));
    }
    Ok(rows)
}

/// The accepted field values in `0..=31` for each format code: the value is put in all three
/// fields, as [`probe_joint`] does, and a completed job is accepted.
pub fn probe_joint<R: Runner>(r: &mut R, fmt: u32) -> io::Result<Vec<u32>> {
    let mut accepted = Vec::new();
    for k in 0..=QTBL.mask {
        let (outcome, _) = run_on(r, MEM_BYTES, fmt, [k, k, k])?;
        if outcome == Outcome::Done {
            accepted.push(k);
        }
    }
    Ok(accepted)
}

/// The candidate chroma placement the written mask shows, for a format and `encoding`. `Ok(None)`
/// if the job did not complete or no candidate predicts the mask.
pub fn probe_chroma<R: Runner>(
    r: &mut R,
    fmt: u32,
    encoding: [u32; 3],
) -> io::Result<Option<(ChromaAt, usize, usize)>> {
    let (outcome, mask) = run_on(r, MEM_BYTES, fmt, encoding)?;
    if outcome != Outcome::Done {
        return Ok(None);
    }
    let Some(end) = mask.iter().rposition(|&w| w).map(|i| i + 1) else {
        return Ok(None);
    };
    for at in ChromaAt::ALL {
        let (start, len) = at.place(LUMA_BYTES);
        let want = |i: usize| i < LUMA_BYTES || (i >= start && i < start + len);
        let matches = (0..MEM_BYTES).all(|i| mask[i] == want(i));
        if matches && end == start + len {
            return Ok(Some((at, start, len)));
        }
    }
    Ok(None)
}

/// What a reach probe found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The smallest dma-buf the job completes in.
    Bounded(usize),
    /// The job completes in an empty dma-buf: the block does not bound its writes. A job check
    /// must not rely on a reach from such a block.
    Unbounded,
    /// The job does not complete even in the probe's largest buffer.
    Failed,
}

/// The reach of the job, by bisection over the output size. Completion is monotone in size: a
/// larger buffer does not fail a job a smaller one completes.
pub fn probe_reach<R: Runner>(r: &mut R, fmt: u32, encoding: [u32; 3]) -> io::Result<Reach> {
    let done = |r: &mut R, len: usize| -> io::Result<bool> {
        Ok(run_on(r, len, fmt, encoding)?.0 == Outcome::Done)
    };
    if done(r, 0)? {
        return Ok(Reach::Unbounded);
    }
    if !done(r, MEM_BYTES)? {
        return Ok(Reach::Failed);
    }
    let (mut lo, mut hi) = (0usize, MEM_BYTES);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if done(r, mid)? {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Ok(Reach::Bounded(hi))
}

/// A runner on a service node. Each run opens a claim on the JPEG decoder, places the output and
/// the fixture input as buffers, submits the job, and reads the status back. The claim closes when
/// the run ends, so every run starts from a fresh session.
///
/// `alloc` makes a dma-buf of the given size: on a board, from the DMA heap; in tests, a sealed
/// memfd. `input` is the fixture: the three tables at [`QTBL_OFF`], [`HMIN_OFF`] and [`HVAL_OFF`],
/// and the stream at [`STREAM_OFF`]. The stream length register is set from it.
pub struct Board<D: Device, A: FnMut(u64) -> io::Result<OwnedFd>> {
    service: Service<D>,
    alloc: A,
    input: Vec<u8>,
    started: Instant,
}

impl<D: Device, A: FnMut(u64) -> io::Result<OwnedFd>> Board<D, A> {
    /// A runner on `device`, with `alloc` and the fixture `input`. `input` must hold the stream at
    /// [`STREAM_OFF`]; [`Runner::run`] refuses a shorter one.
    pub fn new(device: D, alloc: A, input: Vec<u8>) -> Self {
        Self {
            service: Service::new(device),
            alloc,
            input,
            started: Instant::now(),
        }
    }
}

fn other(context: &str, error: impl std::fmt::Debug) -> io::Error {
    io::Error::other(format!("{context}: {error:?}"))
}

impl<D: Device, A: FnMut(u64) -> io::Result<OwnedFd>> Runner for Board<D, A> {
    fn run(&mut self, file: &[u32], mem: &mut [u8]) -> io::Result<u32> {
        let stream = self
            .input
            .len()
            .checked_sub(STREAM_OFF as usize)
            .ok_or_else(|| {
                io::Error::other("the fixture input is shorter than its stream offset")
            })?;
        let mut file = file.to_vec();
        file[REG_STRM_LEN] = u32::try_from(stream)
            .map_err(|_| io::Error::other("the fixture stream is too long"))?
            << 4;

        let out = (self.alloc)(mem.len() as u64)?;
        let input = (self.alloc)(self.input.len() as u64)?;
        let out_map = Mapping::new(out.as_fd(), mem.len(), 0, true)?;
        out_map
            .write_bytes(0, mem)
            .ok_or_else(|| io::Error::other("the output mapping is shorter than the buffer"))?;
        let in_map = Mapping::new(input.as_fd(), self.input.len(), 0, true)?;
        in_map
            .write_bytes(0, &self.input)
            .ok_or_else(|| io::Error::other("the input mapping is shorter than the fixture"))?;

        let now_ns = u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let mut claim = self
            .service
            .claim(Block::JpegDecoder, now_ns)
            .map_err(|e| other("claim", e))?;
        let input_id = claim
            .add_buffer(input)
            .map_err(|e| other("input buffer", e))?;
        let out_id = claim
            .add_buffer(out)
            .map_err(|e| other("output buffer", e))?;

        let addresses = [
            Address {
                register: REG_QTBL as u16,
                buffer: input_id,
                offset: QTBL_OFF,
            },
            Address {
                register: REG_HMIN as u16,
                buffer: input_id,
                offset: HMIN_OFF,
            },
            Address {
                register: REG_HVAL as u16,
                buffer: input_id,
                offset: HVAL_OFF,
            },
            Address {
                register: REG_STRM as u16,
                buffer: input_id,
                offset: STREAM_OFF,
            },
            Address {
                register: REG_OUT as u16,
                buffer: out_id,
                offset: 0,
            },
        ];
        let job = Job {
            identity: 1,
            first: 0,
            registers: &file,
            more: &[],
            addresses: &addresses,
            read: Some(ReadBack { first: 1, count: 1 }),
        };
        claim.submit(&job).map_err(|e| other("submit", e))?;
        let mut status = [0u32; 1];
        claim
            .complete(&mut status)
            .map_err(|e| other("complete", e))?;

        let back = out_map
            .as_slice()
            .get(..mem.len())
            .ok_or_else(|| io::Error::other("the output mapping is shorter than the buffer"))?;
        mem.copy_from_slice(back);
        Ok(status[0])
    }
}
