//! [`Capture`]: the ISP's frame path through `vvcam-isp`, with every register write checked and
//! every address the memory interface is given taken from memory it holds.
//!
//! # The paths by which a caller's numbers reach the block, and the check on each
//!
//! - **A register and a value** ([`Capture::write`]): refused, before any request, unless the
//!   register is in [`access::WRITABLE`](crate::access::WRITABLE) and the value sets no bit
//!   outside its mask. The table holds no address, size, offset counter or path enable.
//! - **A register to read** ([`Capture::read`]): refused unless
//!   [`access::readable`](crate::access::readable).
//! - **How much memory** ([`Capture::provide`]): any length; the host allocates it, and what comes
//!   back is checked by [`FrameMemory::allocate`] (aligned, below 4 GiB, as long as asked).
//! - **The planes** ([`Capture::start`]): offsets and sizes, checked by [`Planes::new`] and then
//!   against every memory in the ring. The registers written from them are each memory's own
//!   address plus a plane's offset, and the plane's size. Every main-path address register
//!   (Y, Cb, Cr, their second set, raw and JPEG) is pointed into the armed memory, even those of
//!   planes the format does not use, so no register the block might read is left naming memory
//!   from before.
//! - **Which memory next** ([`Capture::rearm`]): an index into the ring, checked.
//! - **Reading a frame back** ([`Capture::copy_out`]): an index and a byte range, checked against
//!   the planes and the memory; the read is the host's.
//!
//! The memory interface is only switched on ([`MIV2_CTRL`]) after every address register points
//! into the ring, and the ring cannot be given back while it is on, nor for
//! [`RELEASE_AFTER`] after it is turned off (the frame in flight when it was turned off finishes
//! into memory still held). Dropping a [`Capture`] turns the path off and waits out the same time
//! before the memory goes with it.

use std::io;
use std::time::{Duration, Instant};

use lemnos_driver_k230isp_sys::uapi::{EVENT_ELEMS, EVENT_IRQ_TYPE};
use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::DmaBuffers;

use crate::access::{readable, writable};
use crate::memory::{FrameMemory, MemoryError, Planes};
use crate::regs::{
    MIS_MP_FRAME_END, MIV2_CTRL, MIV2_IMSC, MIV2_MP_RAW_PATH, MIV2_MP_YCBCR_PATH,
    MP_CB_BASE_AD_INIT, MP_CB_BASE_AD_INIT2, MP_CB_OFFS_CNT_INIT, MP_CB_SIZE_INIT,
    MP_CR_BASE_AD_INIT, MP_CR_BASE_AD_INIT2, MP_CR_OFFS_CNT_INIT, MP_CR_SIZE_INIT, MP_CTRL,
    MP_CTRL_AUTO_UPDATE, MP_CTRL_CFG_UPD, MP_CTRL_INIT_BASE_EN, MP_CTRL_INIT_OFFSET_EN,
    MP_JDP_BASE_AD_INIT, MP_JDP_OFFS_CNT_INIT, MP_JDP_SIZE_INIT, MP_RAW_BASE_AD_INIT,
    MP_RAW_BASE_AD_SHD, MP_RAW_OFFS_CNT_INIT, MP_RAW_SIZE_INIT, MP_Y_BASE_AD_INIT,
    MP_Y_BASE_AD_INIT2, MP_Y_BASE_AD_SHD, MP_Y_OFFS_CNT_INIT, MP_Y_SIZE_INIT,
};

#[cfg(not(test))]
use crate::node::IspNode as Port;
#[cfg(test)]
use crate::tests::FakePort as Port;

/// Memories in the ring: one the path writes, one armed to be written next, one being read back.
pub const RING: usize = 3;

/// How long memory is kept after the path is turned off: past the end of the frame in flight at
/// the slowest frame rate the driver expects (one frame a second), twice over.
#[cfg(not(test))]
pub const RELEASE_AFTER: Duration = Duration::from_secs(2);
/// Shortened for the tests, which wait it out.
#[cfg(test)]
pub const RELEASE_AFTER: Duration = Duration::from_millis(50);

/// `VVCAM_EID_MIV2_MIS` (`vvcam_isp.h`): the memory interface's interrupt, carrying `MIV2_MIS`.
pub const EID_MIV2_MIS: u32 = 0x01;

/// The DMA memory a [`Capture`] takes from the host: the same type as a driver's DMA need.
pub type Dma = Box<dyn DmaBuffers<Error = ErrorKind> + Send>;

/// Which of the main path's two outputs runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// The ISP's processed output, through the YCbCr path (YUV and RGB formats).
    YCbCr,
    /// The raw (Bayer) path.
    Raw,
}

impl Output {
    const fn enable(self) -> u32 {
        match self {
            Self::YCbCr => MIV2_MP_YCBCR_PATH,
            Self::Raw => MIV2_MP_RAW_PATH,
        }
    }

    const fn shadow(self) -> u32 {
        match self {
            Self::YCbCr => MP_Y_BASE_AD_SHD,
            Self::Raw => MP_RAW_BASE_AD_SHD,
        }
    }
}

/// Why a [`Capture`] call failed.
#[derive(Debug)]
pub enum CaptureError {
    /// The device refused a request.
    Io(io::Error),
    /// The host's DMA memory refused.
    Dma(ErrorKind),
    /// The memory or the planes failed their check.
    Memory(MemoryError),
    /// A register outside the table, a bit outside a register's mask, or a call that does not
    /// fit the state: no request was made.
    Refused,
}

impl From<io::Error> for CaptureError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// One interrupt of the memory interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MiEvent {
    /// `MIV2_MIS` as the kernel's handler read and cleared it.
    pub status: u32,
    /// When the handler ran, on the kernel's monotonic clock (`ktime_get_ns`).
    pub timestamp_ns: u64,
}

/// The ISP's frame path. See the module documentation for what is checked.
pub struct Capture {
    port: Port,
    dma: Dma,
    memory: [Option<FrameMemory>; RING],
    /// What the path was started with; `Some` while it is on.
    armed: Option<(Output, Planes)>,
    /// When the path was last turned off.
    stopped: Option<Instant>,
}

impl core::fmt::Debug for Capture {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Capture")
            .field("memory", &self.memory)
            .field("armed", &self.armed)
            .finish_non_exhaustive()
    }
}

impl Capture {
    /// Opens the `vvcam-isp` device at `path` (normally `/dev/vvcam-isp.0`; the path is always
    /// the caller's), with `dma` for the frame memory. On failure `dma` comes back with the error.
    #[cfg(not(test))]
    pub fn open(path: impl AsRef<std::path::Path>, dma: Dma) -> Result<Self, (io::Error, Dma)> {
        match Port::open(path.as_ref()) {
            Ok(port) => Ok(Self::with_port(port, dma)),
            Err(e) => Err((e, dma)),
        }
    }

    /// Opens the device at `path` again in place of the one held, and turns the path off
    /// through it: for a caller whose device stopped answering. The memory and its checks stay.
    #[cfg(not(test))]
    pub fn reopen(&mut self, path: impl AsRef<std::path::Path>) -> Result<(), CaptureError> {
        self.port = Port::open(path.as_ref())?;
        self.stop()
    }

    pub(crate) fn with_port(port: Port, dma: Dma) -> Self {
        Self {
            port,
            dma,
            memory: [None, None, None],
            armed: None,
            stopped: None,
        }
    }

    /// The register at `addr`. Refused for one [`readable`] refuses.
    pub fn read(&self, addr: u32) -> Result<u32, CaptureError> {
        if !readable(addr) {
            return Err(CaptureError::Refused);
        }
        Ok(self.port.read_raw(addr)?)
    }

    /// Writes the bits of `value` that [`writable`] allows at `addr`; a register whose mask is not
    /// every bit is read first and its other bits written back as they were. Refused, before any
    /// request, for a register outside the table or a value with a bit outside its mask.
    pub fn write(&self, addr: u32, value: u32) -> Result<(), CaptureError> {
        let Some(mask) = writable(addr) else {
            return Err(CaptureError::Refused);
        };
        if value & !mask != 0 {
            return Err(CaptureError::Refused);
        }
        let value = if mask == u32::MAX {
            value
        } else {
            (self.port.read_raw(addr)? & !mask) | value
        };
        Ok(self.port.write_raw(addr, value)?)
    }

    /// Queues the memory interface's interrupt on this file as events ([`Self::next_event`]) and
    /// lets the main path's frame ends raise it.
    pub fn watch_frames(&self) -> Result<(), CaptureError> {
        self.port.subscribe(EID_MIV2_MIS)?;
        self.write(MIV2_IMSC, MIS_MP_FRAME_END)
    }

    /// The next memory-interface interrupt, or `None` when none is queued. Never blocks;
    /// allocates nothing.
    pub fn next_event(&self) -> Result<Option<MiEvent>, CaptureError> {
        // Only the one id is subscribed; anything else is passed over, at most a queue's worth.
        for _ in 0..=EVENT_ELEMS {
            let Some(e) = self.port.dequeue()? else {
                return Ok(None);
            };
            if e.kind == EVENT_IRQ_TYPE && e.id == EID_MIV2_MIS {
                return Ok(Some(MiEvent {
                    status: e.irq_value,
                    timestamp_ns: e.timestamp,
                }));
            }
        }
        Ok(None)
    }

    /// Makes sure every memory in the ring holds at least `bytes`, keeping memory that does and
    /// replacing memory that doesn't. Refused while the path is on, and replacing is refused for
    /// [`RELEASE_AFTER`] after it was turned off.
    pub fn provide(&mut self, bytes: u64) -> Result<(), CaptureError> {
        if self.armed.is_some() || bytes == 0 {
            return Err(CaptureError::Refused);
        }
        if self.memory.iter().flatten().any(|m| m.len() < bytes) {
            self.release()?;
        }
        for slot in &mut self.memory {
            if slot.is_none() {
                let memory = FrameMemory::allocate(&mut *self.dma, bytes).map_err(|e| match e {
                    Ok(e) => CaptureError::Dma(e),
                    Err(e) => CaptureError::Memory(e),
                })?;
                *slot = Some(memory);
            }
        }
        Ok(())
    }

    /// Gives every memory in the ring back to the host. Refused while the path is on, and for
    /// [`RELEASE_AFTER`] after it was turned off.
    pub fn release(&mut self) -> Result<(), CaptureError> {
        if self.armed.is_some() || !self.quiet() {
            return Err(CaptureError::Refused);
        }
        for slot in &mut self.memory {
            if let Some(memory) = slot.take()
                && let Err(e) = self.dma.release(memory.id())
            {
                *slot = Some(memory);
                return Err(CaptureError::Dma(e));
            }
        }
        Ok(())
    }

    /// Whether the frame in flight when the path was last turned off has surely finished.
    fn quiet(&self) -> bool {
        self.stopped.is_none_or(|at| at.elapsed() >= RELEASE_AFTER)
    }

    /// The ring's memory `slot`, if provided.
    #[must_use]
    pub fn memory(&self, slot: usize) -> Option<&FrameMemory> {
        self.memory.get(slot)?.as_ref()
    }

    /// Whether the path is on.
    #[must_use]
    pub fn running(&self) -> bool {
        self.armed.is_some()
    }

    /// Starts `output` writing `planes`: points every main-path address register at memory 0 and
    /// loads it, points them at memory 1 for the frame after, then switches the path on, and only
    /// that path. Refused while on, without a full ring, or for planes that do not fit every
    /// memory in it.
    pub fn start(&mut self, output: Output, planes: Planes) -> Result<(), CaptureError> {
        if self.armed.is_some() {
            return Err(CaptureError::Refused);
        }
        for slot in &self.memory {
            match slot {
                Some(memory) if planes.fits(memory) => {}
                Some(_) => return Err(CaptureError::Memory(MemoryError::Outside)),
                None => return Err(CaptureError::Refused),
            }
        }
        for slot in 0..RING {
            self.sync_for_device(slot)?;
        }
        // Every path off while the registers change.
        self.port.write_raw(MIV2_CTRL, 0)?;
        self.arm(0, &planes)?;
        let ctrl = MP_CTRL_INIT_OFFSET_EN | MP_CTRL_INIT_BASE_EN | MP_CTRL_AUTO_UPDATE;
        let current = self.port.read_raw(MP_CTRL)?;
        self.port.write_raw(
            MP_CTRL,
            (current & !(ctrl | MP_CTRL_CFG_UPD)) | ctrl | MP_CTRL_CFG_UPD,
        )?;
        self.arm(1, &planes)?;
        // Counted as on whatever the answer: a failed request may still have reached the block,
        // and on is the state that keeps the memory held until `stop` succeeds.
        let on = self.port.write_raw(MIV2_CTRL, output.enable());
        self.armed = Some((output, planes));
        self.stopped = None;
        Ok(on?)
    }

    /// Points the main path at memory `slot` for the frame after the one being written. Refused
    /// when the path is off or `slot` is past the ring.
    pub fn rearm(&mut self, slot: usize) -> Result<(), CaptureError> {
        let Some((_, planes)) = self.armed else {
            return Err(CaptureError::Refused);
        };
        if slot >= RING {
            return Err(CaptureError::Refused);
        }
        self.sync_for_device(slot)?;
        self.arm(slot, &planes)
    }

    /// The memory the path is writing now, from its shadow base register; `None` when the path is
    /// off or the register names none of the ring (as before the first frame starts).
    pub fn active(&self) -> Result<Option<usize>, CaptureError> {
        let Some((output, planes)) = self.armed else {
            return Ok(None);
        };
        let base = self.port.read_raw(output.shadow())?;
        let offset = planes.y().offset;
        Ok((0..RING).find(|&slot| {
            self.memory[slot]
                .as_ref()
                .is_some_and(|m| m.address(offset) == base)
        }))
    }

    /// Takes memory `slot` back from the block before reading it.
    pub fn take(&mut self, slot: usize) -> Result<(), CaptureError> {
        let id = self.held(slot)?.id();
        self.dma.sync_for_cpu(id).map_err(CaptureError::Dma)
    }

    /// Copies `out.len()` bytes from memory `slot` at `offset`. Refused for a range outside the
    /// armed planes or the memory. Allocates nothing.
    pub fn copy_out(
        &mut self,
        slot: usize,
        offset: u64,
        out: &mut [u8],
    ) -> Result<(), CaptureError> {
        let Some((_, planes)) = self.armed else {
            return Err(CaptureError::Refused);
        };
        let memory = self.held(slot)?;
        let end = u64::try_from(out.len())
            .ok()
            .and_then(|n| offset.checked_add(n))
            .ok_or(CaptureError::Refused)?;
        if end > planes.end() || end > memory.len() {
            return Err(CaptureError::Refused);
        }
        let id = memory.id();
        self.dma.read(id, offset, out).map_err(CaptureError::Dma)
    }

    /// Turns every memory-interface path off and updates the main path's registers now. The
    /// memory stays held for [`RELEASE_AFTER`].
    /// Until the write that turns the path off succeeds, the path counts as on: the memory stays
    /// held.
    pub fn stop(&mut self) -> Result<(), CaptureError> {
        self.port.write_raw(MIV2_CTRL, 0)?;
        if self.armed.take().is_some() {
            self.stopped = Some(Instant::now());
        }
        let current = self.port.read_raw(MP_CTRL)?;
        self.port.write_raw(MP_CTRL, current | MP_CTRL_CFG_UPD)?;
        Ok(())
    }

    fn held(&self, slot: usize) -> Result<&FrameMemory, CaptureError> {
        self.memory
            .get(slot)
            .and_then(Option::as_ref)
            .ok_or(CaptureError::Refused)
    }

    fn sync_for_device(&mut self, slot: usize) -> Result<(), CaptureError> {
        let id = self.held(slot)?.id();
        self.dma.sync_for_device(id).map_err(CaptureError::Dma)
    }

    /// Points every main-path address register at memory `slot`'s planes. The caller has checked
    /// the planes against every memory in the ring.
    fn arm(&self, slot: usize, planes: &Planes) -> Result<(), CaptureError> {
        let memory = self.held(slot)?;
        let (y, cb, cr) = (planes.y(), planes.cb_or_y(), planes.cr_or_y());
        let (ya, cba, cra) = (
            memory.address(y.offset),
            memory.address(cb.offset),
            memory.address(cr.offset),
        );
        let writes = [
            (MP_Y_BASE_AD_INIT, ya),
            (MP_Y_SIZE_INIT, y.size),
            (MP_Y_OFFS_CNT_INIT, 0),
            (MP_CB_BASE_AD_INIT, cba),
            (MP_CB_SIZE_INIT, cb.size),
            (MP_CB_OFFS_CNT_INIT, 0),
            (MP_CR_BASE_AD_INIT, cra),
            (MP_CR_SIZE_INIT, cr.size),
            (MP_CR_OFFS_CNT_INIT, 0),
            (MP_Y_BASE_AD_INIT2, ya),
            (MP_CB_BASE_AD_INIT2, cba),
            (MP_CR_BASE_AD_INIT2, cra),
            (MP_RAW_BASE_AD_INIT, ya),
            (MP_RAW_SIZE_INIT, y.size),
            (MP_RAW_OFFS_CNT_INIT, 0),
            (MP_JDP_BASE_AD_INIT, ya),
            (MP_JDP_SIZE_INIT, y.size),
            (MP_JDP_OFFS_CNT_INIT, 0),
        ];
        for (addr, value) in writes {
            self.port.write_raw(addr, value)?;
        }
        Ok(())
    }
}

impl Drop for Capture {
    /// Turns the path off and keeps the memory until the frame in flight has surely finished, so
    /// the host never gets it back while the block may still write it.
    fn drop(&mut self) {
        if self.armed.is_some() {
            let _ = self.stop();
        }
        if let Some(at) = self.stopped
            && let Some(left) = RELEASE_AFTER.checked_sub(at.elapsed())
        {
            std::thread::sleep(left);
        }
        let _ = self.release();
    }
}
