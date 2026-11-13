//! The block as the driver sees it ([`Hardware`]), and the two ways to it the board file chooses
//! between: the vendor kernel's `vvcam-isp` device ([`LinuxHardware`]), or the block's registers
//! and interrupt from the host, which on Linux are the Lemnos kernel module's (`regs:`, `irq:`,
//! and optionally `clocks:` and `resets:`; [`ModuleHardware`]).
//!
//! On either way every register access is checked against `lemnos-driver-k230isp-sys`'s table
//! before it reaches the block ([`IspNode`](lemnos_driver_k230isp_sys::IspNode) on the vendor
//! device, [`ModuleHardware`] itself on the module's registers); [`Hardware`] adds nothing that
//! could widen it.

#[cfg(target_os = "linux")]
use std::io;
use std::time::Duration;

use lemnos_core::{
    ClocksHandle, InterruptHandle, RegistersHandle, ResetHandle, Result, Status, status_of,
};
use lemnos_driver_k230isp_sys::access::{allowed, readable, writable};
use lemnos_driver_k230isp_sys::regs::{ISP_ICR, ISP_IMSC, ISP_MIS, MIS_FRAME_OUT, WINDOW_BYTES};
use lemnos_hal::blocks::{enable_clocks, pulse_resets};

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

/// The block's own resources, from the host: on Linux, the Lemnos kernel module's, with the ISP
/// bound to it in place of the vendor driver.
///
/// - `regs`: the ISP's register window, at least [`WINDOW_BYTES`] long. Every read and write is
///   checked against the sys crate's table exactly as on the vendor device: the table holds no
///   register that takes an address, a length or a DMA control.
/// - `irq`: the ISP core's interrupt. The vendor driver's handler reads and clears `ISP_MIS`; here
///   the driver does, when the host reports the interrupt: it reads [`ISP_MIS`], writes it to
///   [`ISP_ICR`] and enables the interrupt again.
/// - `clocks`, `resets` (both optional): every clock the board names is turned on at each open,
///   and every reset line asserted, then released, so the block starts from its reset state.
///   Clocks stay on across a close: turning them off is the host's, when it lets the block go.
///
/// Nothing here allocates.
pub struct ModuleHardware {
    regs: RegistersHandle,
    irq: InterruptHandle,
    clocks: Option<ClocksHandle>,
    resets: Option<ResetHandle>,
    open: bool,
}

impl ModuleHardware {
    /// The block through `regs` and `irq`, and the `clocks` and `resets` the board names.
    #[must_use]
    pub fn new(
        regs: RegistersHandle,
        irq: InterruptHandle,
        clocks: Option<ClocksHandle>,
        resets: Option<ResetHandle>,
    ) -> Self {
        Self {
            regs,
            irq,
            clocks,
            resets,
            open: false,
        }
    }

    fn opened(&self) -> Result<()> {
        if self.open { Ok(()) } else { Err(Status::IO) }
    }

    fn raw_read(&mut self, addr: u32) -> Result<u32> {
        self.regs.read32(addr as usize).map_err(status_of)
    }

    fn raw_write(&mut self, addr: u32, value: u32) -> Result<()> {
        self.regs.write32(addr as usize, value).map_err(status_of)
    }
}

impl Hardware for ModuleHardware {
    fn open(&mut self) -> Result<()> {
        self.open = false;
        if self.regs.size() < WINDOW_BYTES as usize {
            // The board named a region that isn't the ISP's whole window.
            return Err(Status::INVALID_ARGUMENT);
        }
        if let Some(clocks) = self.clocks.as_mut() {
            enable_clocks(&mut **clocks).map_err(status_of)?;
        }
        if let Some(resets) = self.resets.as_mut() {
            pulse_resets(&mut **resets).map_err(status_of)?;
        }
        // What is pending from before is not this open's.
        let pending = self.raw_read(ISP_MIS)?;
        self.raw_write(ISP_ICR, pending)?;
        let mask = self.raw_read(ISP_IMSC)?;
        if mask & MIS_FRAME_OUT == 0 {
            self.raw_write(ISP_IMSC, mask | MIS_FRAME_OUT)?;
        }
        self.irq.enable().map_err(status_of)?;
        self.open = true;
        Ok(())
    }

    fn close(&mut self) {
        if self.open {
            let _ = self.irq.disable();
        }
        self.open = false;
    }

    fn read(&mut self, addr: u32) -> Result<u32> {
        self.opened()?;
        if !readable(addr) {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.raw_read(addr)
    }

    fn write(&mut self, addr: u32, value: u32) -> Result<()> {
        self.opened()?;
        let Some(mask) = writable(addr) else {
            return Err(Status::INVALID_ARGUMENT);
        };
        if !allowed(addr, value) {
            return Err(Status::INVALID_ARGUMENT);
        }
        if mask == u32::MAX {
            self.raw_write(addr, value)
        } else {
            self.regs
                .modify32(addr as usize, mask, value)
                .map_err(status_of)
        }
    }

    fn next_event(&mut self) -> Result<Option<Event>> {
        self.opened()?;
        let Some(event) = self.irq.wait(Some(Duration::ZERO)).map_err(status_of)? else {
            return Ok(None);
        };
        let status = self.raw_read(ISP_MIS)?;
        self.raw_write(ISP_ICR, status)?;
        self.irq.enable().map_err(status_of)?;
        Ok(Some(Event {
            status,
            timestamp_ns: event.timestamp_ns,
        }))
    }
}
