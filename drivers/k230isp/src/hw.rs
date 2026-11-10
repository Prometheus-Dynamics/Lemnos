//! The block as the driver sees it ([`Hardware`]), and the Linux implementation over the vendor
//! kernel's `vvcam-isp` device ([`LinuxHardware`]).
//!
//! Every register access goes through `lemnos-driver-k230isp-sys`'s `IspNode`, which refuses a
//! register outside its table before any request reaches the device; [`Hardware`] adds nothing
//! that could widen it.

#[cfg(target_os = "linux")]
use std::io;

use lemnos_core::{Result, Status};

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
