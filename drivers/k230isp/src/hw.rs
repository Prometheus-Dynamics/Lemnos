//! The block as the driver sees it ([`Hardware`]), and the two ways to it the board file chooses
//! between: the vendor kernel's `vvcam-isp` device ([`LinuxHardware`]), or the configuration
//! role of the block as `lemnos.ko` drives it ([`ModuleHardware`]), beside the frames role
//! another driver holds.
//!
//! On either way every register access is checked against `lemnos-driver-k230isp-sys`'s table
//! before it reaches the block ([`IspNode`](lemnos_driver_k230isp_sys::IspNode) on the vendor
//! device, [`ModuleHardware`] itself, and the module after it); [`Hardware`] adds nothing that
//! could widen it.

#[cfg(target_os = "linux")]
use std::io;

use lemnos_core::{Result, Status};
#[cfg(target_os = "linux")]
use lemnos_driver_k230isp_sys::access::{allowed, readable};

/// The vendor driver's device node, the default for the `device` setting.
pub const DEVICE_PATH: &str = "/dev/vvcam-isp.0";

/// One interrupt of the ISP core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// The masked interrupt status the kernel's handler read and cleared (`ISP_MIS`).
    pub status: u32,
    /// When the handler ran, on the kernel's monotonic clock.
    pub timestamp_ns: u64,
}

/// What the driver asks of the block. The real one is [`LinuxHardware`]; tests use a stand-in.
pub trait Hardware: Send {
    /// Opens the device, queues its interrupt as events and lets a frame leaving the ISP raise
    /// it. Opening an open device starts over.
    fn open(&mut self) -> Result<()>;
    /// Closes the device. Nothing open is not an error.
    fn close(&mut self);
    /// The register at `addr`.
    fn read(&mut self, addr: u32) -> Result<u32>;
    /// Writes `value` to the register at `addr`.
    fn write(&mut self, addr: u32, value: u32) -> Result<()>;
    /// The next interrupt, if one is queued. Never blocks; allocates nothing.
    fn next_event(&mut self) -> Result<Option<Event>>;
}

/// Where the device is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    path: String,
}

impl Settings {
    /// The device at `path`, or [`DEVICE_PATH`]. `INVALID_ARGUMENT` for an empty path.
    pub fn new(path: Option<&str>) -> Result<Self> {
        match path {
            Some("") => Err(Status::INVALID_ARGUMENT),
            Some(p) => Ok(Self { path: p.to_owned() }),
            None => Ok(Self {
                path: DEVICE_PATH.to_owned(),
            }),
        }
    }

    /// The device's path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

#[cfg(target_os = "linux")]
fn status(e: &io::Error) -> Status {
    match e.kind() {
        io::ErrorKind::NotFound => Status::NO_DEVICE,
        io::ErrorKind::InvalidInput => Status::INVALID_ARGUMENT,
        _ => Status::IO,
    }
}

/// The vendor kernel's `vvcam-isp` device.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct LinuxHardware {
    settings: Settings,
    node: Option<lemnos_driver_k230isp_sys::IspNode>,
}

#[cfg(target_os = "linux")]
impl LinuxHardware {
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            node: None,
        }
    }

    fn node(&self) -> Result<&lemnos_driver_k230isp_sys::IspNode> {
        self.node.as_ref().ok_or(Status::IO)
    }
}

#[cfg(target_os = "linux")]
impl Hardware for LinuxHardware {
    fn open(&mut self) -> Result<()> {
        self.node = None;
        let node = lemnos_driver_k230isp_sys::IspNode::open(&self.settings.path)
            .map_err(|e| status(&e))?;
        node.watch_frames().map_err(|e| status(&e))?;
        self.node = Some(node);
        Ok(())
    }

    fn close(&mut self) {
        self.node = None;
    }

    fn read(&mut self, addr: u32) -> Result<u32> {
        self.node()?.read(addr).map_err(|e| status(&e))
    }

    fn write(&mut self, addr: u32, value: u32) -> Result<()> {
        self.node()?.write(addr, value).map_err(|e| status(&e))
    }

    fn next_event(&mut self) -> Result<Option<Event>> {
        use lemnos_driver_k230isp_sys::uapi::{EID_ISP_MIS, EVENT_ELEMS, EVENT_IRQ_TYPE};
        let node = self.node()?;
        // Only the ISP core's interrupt is subscribed; anything else is passed over, at most a
        // queue's worth.
        for _ in 0..=EVENT_ELEMS {
            let Some(e) = node.next_event().map_err(|e| status(&e))? else {
                return Ok(None);
            };
            if e.kind == EVENT_IRQ_TYPE && e.id == EID_ISP_MIS {
                return Ok(Some(Event {
                    status: e.irq_value,
                    timestamp_ns: e.timestamp,
                }));
            }
        }
        Ok(None)
    }
}

/// The configuration session with `lemnos.ko`, one request each. The Linux one is
/// [`LinuxConfigSession`]; tests play one back.
#[cfg(target_os = "linux")]
pub trait ConfigSession: Send {
    /// Opens the session, closing one held.
    fn open(&mut self) -> io::Result<()>;
    /// Closes it. What it wrote stays.
    fn close(&mut self);
    /// Writes `writes` in order, then reads `reads` into `values`, at once.
    fn write(
        &mut self,
        writes: &[lemnos_driver_regjob_sys::Write],
        reads: &[u32],
        values: &mut [u32],
    ) -> io::Result<()>;
    /// The next queued interrupt, or `None`. Never blocks.
    fn next_event(&mut self) -> io::Result<Option<lemnos_driver_regjob_sys::Event>>;
}

/// The configuration session on Linux: the block whose device name is `block`, through the
/// module's control node.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct LinuxConfigSession {
    node: std::path::PathBuf,
    block: String,
    session: Option<lemnos_driver_regjob_sys::Session>,
}

#[cfg(target_os = "linux")]
impl LinuxConfigSession {
    /// The block named `block`, through the control node at `node`. Opens nothing yet.
    #[must_use]
    pub fn new(node: std::path::PathBuf, block: String) -> Self {
        Self {
            node,
            block,
            session: None,
        }
    }

    fn session(&self) -> io::Result<&lemnos_driver_regjob_sys::Session> {
        self.session
            .as_ref()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotConnected))
    }
}

#[cfg(target_os = "linux")]
impl ConfigSession for LinuxConfigSession {
    fn open(&mut self) -> io::Result<()> {
        use lemnos_driver_regjob_sys::{Control, PROFILE_K230_ISP};
        self.session = None;
        let control = Control::open(&self.node)?;
        let session = control.open_config(control.find_block(&self.block)?)?;
        if session.profile() != PROFILE_K230_ISP {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.session = Some(session);
        Ok(())
    }

    fn close(&mut self) {
        self.session = None;
    }

    fn write(
        &mut self,
        writes: &[lemnos_driver_regjob_sys::Write],
        reads: &[u32],
        values: &mut [u32],
    ) -> io::Result<()> {
        self.session()?.write(writes, reads, values)
    }

    fn next_event(&mut self) -> io::Result<Option<lemnos_driver_regjob_sys::Event>> {
        self.session()?.next_event()
    }
}

/// The ISP driven by `lemnos.ko` (bound to it, listed in its `k230_isp` parameter), through the
/// block's configuration role: the module's other role, the frames, is another driver's
/// (`k230-capture`), and both run on the block at once.
///
/// - Every read and write is checked against the sys crate's table here, exactly as on the
///   vendor device, and again by the module against its own copy of the same table: neither
///   holds a register that takes an address, a length or a DMA control. `isp_ctrl` is shared
///   with the frames role: the module writes only this role's bits of it (gamma out and the two
///   update strobes) and keeps the frames role's.
/// - The ISP core's frame-out interrupt is the module's, which reads and clears `ISP_MIS` in its
///   handler and queues the status with the time. A reset of the block by the frames role (its
///   frame didn't come, or its session closed on a running path) comes as an event too: the
///   registers written before are gone, so it is an error here, and the driver starts over.
/// - The clocks and the reset line are the module's: it turns the clocks on when the block binds
///   and resets the block when a frame job fails.
///
/// Nothing here allocates.
#[cfg(target_os = "linux")]
pub struct ModuleHardware {
    session: Box<dyn ConfigSession>,
    open: bool,
}

#[cfg(target_os = "linux")]
impl ModuleHardware {
    /// The block through `session`.
    #[must_use]
    pub fn new(session: Box<dyn ConfigSession>) -> Self {
        Self {
            session,
            open: false,
        }
    }

    fn opened(&self) -> Result<()> {
        if self.open { Ok(()) } else { Err(Status::IO) }
    }
}

#[cfg(target_os = "linux")]
impl Hardware for ModuleHardware {
    fn open(&mut self) -> Result<()> {
        self.open = false;
        self.session.open().map_err(|e| status(&e))?;
        // What was queued before is not this open's.
        for _ in 0..=lemnos_driver_regjob_sys::EVENTS {
            if self.session.next_event().map_err(|e| status(&e))?.is_none() {
                break;
            }
        }
        self.open = true;
        Ok(())
    }

    fn close(&mut self) {
        self.session.close();
        self.open = false;
    }

    fn read(&mut self, addr: u32) -> Result<u32> {
        self.opened()?;
        if !readable(addr) {
            return Err(Status::INVALID_ARGUMENT);
        }
        let mut value = [0];
        self.session
            .write(&[], &[addr], &mut value)
            .map_err(|e| status(&e))?;
        Ok(value[0])
    }

    fn write(&mut self, addr: u32, value: u32) -> Result<()> {
        self.opened()?;
        if !allowed(addr, value) {
            return Err(Status::INVALID_ARGUMENT);
        }
        let write = lemnos_driver_regjob_sys::Write {
            offset: addr,
            value,
        };
        self.session
            .write(&[write], &[], &mut [])
            .map_err(|e| status(&e))
    }

    fn next_event(&mut self) -> Result<Option<Event>> {
        self.opened()?;
        let Some(event) = self.session.next_event().map_err(|e| status(&e))? else {
            return Ok(None);
        };
        if event.status & lemnos_driver_regjob_sys::EVENT_RESET != 0 {
            // The frames role reset the block: what this driver wrote is gone.
            return Err(Status::IO);
        }
        Ok(Some(Event {
            status: event.status,
            timestamp_ns: event.timestamp_ns,
        }))
    }
}
