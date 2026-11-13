//! The ISP driven by `lemnos.ko` ([`ModuleHardware`]): bound to the module and listed in its
//! `k230_isp` parameter, in place of the vendor's `vvcam-isp`. This driver holds the block's
//! frames role; the configuration role (`k230isp`, for tuning and statistics) runs on the same
//! block at the same time.
//!
//! What the module does, and this driver does not: it writes every address register of the main
//! path (Y, Cb, Cr, their second set, raw and JPEG) itself, from a dma-buf the session imported
//! and a range checked to lie inside it, and the size each plane wraps at from the same range;
//! it switches the running path to a job's memory only at the end of a frame, in its interrupt
//! handler, and reads back the shadow registers to see that the block took it; it keeps a
//! buffer pinned while the block may still write it, and resets the block (its reset line)
//! before it lets one go when a session ends on a running path; it checks every other register
//! write against its own table, and writes `isp_ctrl` by read-modify-write so the configuration
//! role's bits stay. See `kernel/lemnos_regjob.h`.
//!
//! The ring maps onto the module's jobs: a job is "write memory `slot` from the next frame on".
//! [`Hardware::start`] sends the first, which an idle block takes at once, and the next job
//! (memory 1) goes as soon as the first completes, at the first frame's end; from then on each
//! job is sent when [`Hardware::rearm`] names its memory, and completes at the frame end where
//! the block takes it, which is the frame end [`Hardware::next_event`] reports. A job the block
//! didn't take (the shadows still name the old memory) is sent again. The memory is the
//! module's, allocated for the ISP (so its addresses are the ISP's own) and kept from one start
//! to the next; it is coherent, so taking it back before a read is nothing.
//!
//! Nothing on the frame path allocates.

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;

use lemnos_core::{Result, Status};
use lemnos_driver_k230_capture_sys::access::writable;
use lemnos_driver_k230_capture_sys::mipi::DevAttr;
use lemnos_driver_k230_capture_sys::regs::{
    MIV2_MP_RAW_PATH, MIV2_MP_YCBCR_PATH, MP_CB_BASE_AD_INIT, MP_CR_BASE_AD_INIT,
    MP_RAW_BASE_AD_INIT, MP_Y_BASE_AD_INIT,
};
use lemnos_driver_k230_capture_sys::{MiEvent, MipiNode, Output, Plane, Planes, RING};
use lemnos_driver_regjob_sys::{self as regjob, Addr, Done, Job, STATUS_UNLATCHED, Write};
use lemnos_linux_sys::mmap::Mapping;

use crate::hw::{Hardware, Settings};

/// The module's control node when the board file names none.
pub const NODE: &str = "/dev/lemnos";
/// How long a frame may take to come before the module resets the block.
pub const FRAME_TIMEOUT_MS: u32 = 2000;

/// The frames session with the module, and the ring's memory, one request each. The Linux one
/// is [`LinuxFrameSession`]; tests play one back.
pub trait FrameSession: Send {
    /// Opens the session, closing one held: the module resets the block if it may still be
    /// writing a buffer of the old one, then lets them go.
    fn open(&mut self) -> io::Result<()>;
    /// Makes memory `slot` of the ring hold at least `bytes`, from the module's memory for the
    /// block. Memory that does is kept; other memory is replaced, and must not be imported.
    fn provide(&mut self, slot: usize, bytes: u64) -> io::Result<()>;
    /// Imports memory `slot` into the session: the index a job's addresses name.
    fn import(&mut self, slot: usize) -> io::Result<u32>;
    /// Writes `writes` at once, checked by the module against the frames role's table.
    fn write(&mut self, writes: &[Write]) -> io::Result<()>;
    /// Starts `job`.
    fn start(&mut self, job: &Job<'_>) -> io::Result<()>;
    /// The started job's end, if it has come.
    fn collect(&mut self) -> io::Result<Option<Done>>;
    /// Copies from memory `slot` at `offset` into `out`. Allocates nothing.
    fn copy_out(&mut self, slot: usize, offset: u64, out: &mut [u8]) -> io::Result<()>;
}

fn io_status(e: &io::Error) -> Status {
    match e.kind() {
        io::ErrorKind::NotFound => Status::NO_DEVICE,
        io::ErrorKind::InvalidInput => Status::INVALID_ARGUMENT,
        _ => Status::IO,
    }
}

/// The ISP through `lemnos.ko`'s frames role, and the vendor's `vvcam-mipi` for the receiver.
pub struct ModuleHardware {
    session: Box<dyn FrameSession>,
    mipi_path: String,
    mipi: Option<MipiNode>,
    /// The session is open.
    open: bool,
    /// Bytes every memory of the ring holds, once provided.
    bytes: u64,
    /// Each memory's index in the session, once imported.
    imported: [Option<u32>; RING],
    /// What the path was started with; `Some` while it runs.
    armed: Option<(Output, Planes)>,
    /// The memory the block took last: the one it writes now.
    latched: Option<usize>,
    /// The memory of the job with the module.
    pending: Option<usize>,
    /// The memory to send once the pending job completes.
    next: Option<usize>,
}

impl ModuleHardware {
    /// The ISP through `session`, the CSI device at `settings`' `mipi`. Touches nothing yet.
    #[must_use]
    pub fn new(settings: &Settings, session: Box<dyn FrameSession>) -> Self {
        Self {
            session,
            mipi_path: settings.mipi().to_owned(),
            mipi: None,
            open: false,
            bytes: 0,
            imported: [None; RING],
            armed: None,
            latched: None,
            pending: None,
            next: None,
        }
    }

    fn forget(&mut self) {
        self.imported = [None; RING];
        self.armed = None;
        self.latched = None;
        self.pending = None;
        self.next = None;
    }

    fn opened(&self) -> Result<()> {
        if self.open { Ok(()) } else { Err(Status::IO) }
    }

    /// Sends the job that points the path at memory `slot`. A refusal is the driver's bug or a
    /// session the module ended: either way the device failed.
    fn send(&mut self, slot: usize) -> Result<()> {
        let Some((output, planes)) = self.armed else {
            return Err(Status::IO);
        };
        let Some(index) = self.imported.get(slot).copied().flatten() else {
            return Err(Status::IO);
        };
        let addr = |reg: u32, plane: Plane| Addr {
            reg,
            buffer: index,
            offset: u64::from(plane.offset),
            bytes: u64::from(plane.size),
        };
        let addrs = [
            addr(MP_Y_BASE_AD_INIT, planes.y()),
            addr(MP_CB_BASE_AD_INIT, planes.cb_or_y()),
            addr(MP_CR_BASE_AD_INIT, planes.cr_or_y()),
            // The raw output's plane is the only one, the first.
            addr(MP_RAW_BASE_AD_INIT, planes.y()),
        ];
        let job = Job {
            writes: &[],
            addrs: &addrs,
            reads: &[],
            // `miv2_ctrl`'s enable for the one output that runs.
            start: match output {
                Output::YCbCr => MIV2_MP_YCBCR_PATH,
                Output::Raw => MIV2_MP_RAW_PATH,
            },
            timeout_ms: FRAME_TIMEOUT_MS,
        };
        self.session.start(&job).map_err(|e| io_status(&e))?;
        self.pending = Some(slot);
        Ok(())
    }
}

impl Hardware for ModuleHardware {
    fn open(&mut self) -> Result<()> {
        self.open = false;
        self.mipi = None;
        self.forget();
        self.session.open().map_err(|e| io_status(&e))?;
        self.open = true;
        self.mipi = Some(MipiNode::open(&self.mipi_path).map_err(|e| io_status(&e))?);
        Ok(())
    }

    fn set_link(&mut self, attr: &DevAttr) -> Result<()> {
        self.mipi
            .as_ref()
            .ok_or(Status::IO)?
            .set_link(attr)
            .map_err(|e| io_status(&e))
    }

    fn write(&mut self, addr: u32, value: u32) -> Result<()> {
        self.opened()?;
        match writable(addr) {
            Some(mask) if value & !mask == 0 => {}
            _ => return Err(Status::INVALID_ARGUMENT),
        }
        self.session
            .write(&[Write {
                offset: addr,
                value,
            }])
            .map_err(|e| io_status(&e))
    }

    fn provide(&mut self, bytes: u64) -> Result<()> {
        self.opened()?;
        // Memory is imported only while the path runs, and a session that ran was opened again
        // (the block reset) before it stopped: no memory replaced here is the block's.
        if self.armed.is_some() || bytes == 0 {
            return Err(Status::BUSY);
        }
        for slot in 0..RING {
            self.session
                .provide(slot, bytes)
                .map_err(|e| io_status(&e))?;
        }
        self.bytes = self.bytes.max(bytes);
        Ok(())
    }

    fn start(&mut self, output: Output, planes: Planes) -> Result<()> {
        self.opened()?;
        if self.armed.is_some() {
            return Err(Status::BUSY);
        }
        if self.bytes == 0 || planes.end() > self.bytes {
            return Err(Status::INVALID_ARGUMENT);
        }
        for slot in 0..RING {
            if self.imported[slot].is_none() {
                let index = self.session.import(slot).map_err(|e| io_status(&e))?;
                self.imported[slot] = Some(index);
            }
        }
        self.armed = Some((output, planes));
        // Memory 0 now (the block is idle, so it takes it at once), memory 1 after the first
        // frame: as the driver's ring starts.
        self.send(0)?;
        self.latched = Some(0);
        self.next = Some(1);
        Ok(())
    }

    fn rearm(&mut self, slot: usize) -> Result<()> {
        if self.armed.is_none() || slot >= RING {
            return Err(Status::BUSY);
        }
        if self.pending.is_some() {
            self.next = Some(slot);
            return Ok(());
        }
        self.send(slot)
    }

    fn active(&mut self) -> Result<Option<usize>> {
        Ok(self.armed.and(self.latched))
    }

    fn take(&mut self, slot: usize) -> Result<()> {
        // Coherent memory: nothing to hand back.
        if slot < RING && self.imported[slot].is_some() {
            Ok(())
        } else {
            Err(Status::BUSY)
        }
    }

    fn copy_out(&mut self, slot: usize, offset: u64, out: &mut [u8]) -> Result<()> {
        let Some((_, planes)) = self.armed else {
            return Err(Status::BUSY);
        };
        let end = u64::try_from(out.len())
            .ok()
            .and_then(|n| offset.checked_add(n))
            .ok_or(Status::INVALID_ARGUMENT)?;
        if slot >= RING || end > planes.end() || end > self.bytes {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.session
            .copy_out(slot, offset, out)
            .map_err(|e| io_status(&e))
    }

    /// The path off: a session that may still be writing a buffer is opened again, which has
    /// the module reset the block first. An idle one is kept.
    fn stop(&mut self) -> Result<()> {
        if !self.open || (self.armed.is_none() && self.pending.is_none()) {
            return Ok(());
        }
        self.forget();
        self.open = false;
        self.session.open().map_err(|e| io_status(&e))?;
        self.open = true;
        Ok(())
    }

    fn next_event(&mut self) -> Result<Option<MiEvent>> {
        let Some(slot) = self.pending else {
            return Ok(None);
        };
        let done = match self.session.collect() {
            Ok(Some(done)) => done,
            Ok(None) => return Ok(None),
            // A frame that didn't come (the module reset the block), or a session it ended.
            Err(_) => return Err(Status::IO),
        };
        self.pending = None;
        if done.status & STATUS_UNLATCHED != 0 {
            // The block went on with the memory it had: the same memory is asked for again.
            self.send(slot)?;
        } else {
            self.latched = Some(slot);
            if let Some(next) = self.next.take() {
                self.send(next)?;
            }
        }
        Ok(Some(MiEvent {
            status: done.status & !STATUS_UNLATCHED,
            timestamp_ns: done.timestamp_ns,
        }))
    }
}

/// One memory of the ring: the module's allocation for the ISP, mapped for reading.
#[derive(Debug)]
struct Memory {
    fd: OwnedFd,
    map: Mapping,
}

/// The frames session on Linux, over `lemnos-driver-regjob-sys`, with the ring's memory from
/// the module's allocator for the same block.
#[derive(Debug)]
pub struct LinuxFrameSession {
    node: PathBuf,
    block: String,
    /// The control node and the block's number, from the first open on.
    control: Option<(regjob::Control, u32)>,
    session: Option<regjob::Session>,
    ring: [Option<Memory>; RING],
}

impl LinuxFrameSession {
    /// The block whose device name is `block`, through the control node at `node`. Opens
    /// nothing yet.
    #[must_use]
    pub fn new(node: PathBuf, block: String) -> Self {
        Self {
            node,
            block,
            control: None,
            session: None,
            ring: [None, None, None],
        }
    }

    fn session(&self) -> io::Result<&regjob::Session> {
        self.session
            .as_ref()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotConnected))
    }
}

impl FrameSession for LinuxFrameSession {
    fn open(&mut self) -> io::Result<()> {
        // The old session first: the module resets the block before the new one exists.
        self.session = None;
        if self.control.is_none() {
            let control = regjob::Control::open(&self.node)?;
            let id = control.find_block(&self.block)?;
            self.control = Some((control, id));
        }
        let Some((control, id)) = self.control.as_ref() else {
            return Err(io::ErrorKind::NotConnected.into());
        };
        let session = control.open_session(*id)?;
        if session.profile() != regjob::PROFILE_K230_ISP {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.session = Some(session);
        Ok(())
    }

    fn provide(&mut self, slot: usize, bytes: u64) -> io::Result<()> {
        let Some((control, id)) = self.control.as_ref() else {
            return Err(io::ErrorKind::NotConnected.into());
        };
        let into = self
            .ring
            .get_mut(slot)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        if into.as_ref().is_some_and(|m| m.map.len() as u64 >= bytes) {
            return Ok(());
        }
        *into = None;
        let memory = lemnos_linux_sys::module::alloc(control.node(), *id, bytes)?;
        let len = usize::try_from(memory.size)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let map = Mapping::new(memory.fd.as_fd(), len, 0, false)?;
        *into = Some(Memory { fd: memory.fd, map });
        Ok(())
    }

    fn import(&mut self, slot: usize) -> io::Result<u32> {
        let memory = self
            .ring
            .get(slot)
            .and_then(Option::as_ref)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        Ok(self.session()?.import(memory.fd.as_fd())?.index)
    }

    fn write(&mut self, writes: &[Write]) -> io::Result<()> {
        self.session()?.write(writes, &[], &mut [])
    }

    fn start(&mut self, job: &Job<'_>) -> io::Result<()> {
        self.session()?.start(job)
    }

    fn collect(&mut self) -> io::Result<Option<Done>> {
        self.session()?.collect(&mut [])
    }

    fn copy_out(&mut self, slot: usize, offset: u64, out: &mut [u8]) -> io::Result<()> {
        let memory = self
            .ring
            .get(slot)
            .and_then(Option::as_ref)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let from = usize::try_from(offset).map_err(|_| io::ErrorKind::InvalidInput)?;
        let src = from
            .checked_add(out.len())
            .and_then(|end| memory.map.as_slice().get(from..end))
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        out.copy_from_slice(src);
        Ok(())
    }
}
