//! The block as the driver sees it ([`Hardware`]), and the Linux implementation over the sys
//! crate's [`Capture`](lemnos_driver_k230_capture_sys::Capture) and
//! [`MipiNode`](lemnos_driver_k230_capture_sys::MipiNode) ([`LinuxHardware`]).
//!
//! [`Hardware`] adds nothing that could widen what the sys crate allows: register writes go
//! through its table, and the memory interface is armed only from the memory it holds.

use lemnos_core::{Result, Status};
use lemnos_driver_k230_capture_sys::mipi::DevAttr;
use lemnos_driver_k230_capture_sys::{MiEvent, Output, Planes};

/// The vendor driver's ISP node, the default for the `isp` setting.
pub const ISP_PATH: &str = "/dev/vvcam-isp.0";
/// The vendor driver's CSI node, the default for the `mipi` setting.
pub const MIPI_PATH: &str = "/dev/vvcam-mipi.0";

/// What the driver asks of the block. The real one is [`LinuxHardware`]; tests use a stand-in.
pub trait Hardware: Send {
    /// Opens both devices, or opens them again (turning the memory interface off through the new
    /// handle), and queues the main path's frame ends as events.
    fn open(&mut self) -> Result<()>;
    /// Hands the CSI device the link.
    fn set_link(&mut self, attr: &DevAttr) -> Result<()>;
    /// Writes a frame-path register, through the sys crate's table.
    fn write(&mut self, addr: u32, value: u32) -> Result<()>;
    /// Makes sure each memory of the ring holds `bytes`.
    fn provide(&mut self, bytes: u64) -> Result<()>;
    /// Arms the ring with `planes` and turns `output` on.
    fn start(&mut self, output: Output, planes: Planes) -> Result<()>;
    /// Points the path at memory `slot` for the frame after the one being written.
    fn rearm(&mut self, slot: usize) -> Result<()>;
    /// The memory the path writes now.
    fn active(&mut self) -> Result<Option<usize>>;
    /// Takes memory `slot` back from the block before it is read.
    fn take(&mut self, slot: usize) -> Result<()>;
    /// Copies from memory `slot` at `offset` into `out`. Allocates nothing.
    fn copy_out(&mut self, slot: usize, offset: u64, out: &mut [u8]) -> Result<()>;
    /// Turns the memory interface off.
    fn stop(&mut self) -> Result<()>;
    /// The next frame-end interrupt, if one is queued. Never blocks; allocates nothing.
    fn next_event(&mut self) -> Result<Option<MiEvent>>;
}

/// Where the devices are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    isp: String,
    mipi: String,
}

impl Settings {
    /// The devices at `isp` and `mipi`, or [`ISP_PATH`] and [`MIPI_PATH`]. `INVALID_ARGUMENT`
    /// for an empty path.
    pub fn new(isp: Option<&str>, mipi: Option<&str>) -> Result<Self> {
        let path = |p: Option<&str>, default: &str| match p {
            Some("") => Err(Status::INVALID_ARGUMENT),
            Some(p) => Ok(p.to_owned()),
            None => Ok(default.to_owned()),
        };
        Ok(Self {
            isp: path(isp, ISP_PATH)?,
            mipi: path(mipi, MIPI_PATH)?,
        })
    }

    /// The ISP device's path.
    #[must_use]
    pub fn isp(&self) -> &str {
        &self.isp
    }

    /// The CSI device's path.
    #[must_use]
    pub fn mipi(&self) -> &str {
        &self.mipi
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io;

    use lemnos_core::{Result, Status};
    use lemnos_driver_k230_capture_sys::mipi::DevAttr;
    use lemnos_driver_k230_capture_sys::{
        Capture, CaptureError, Dma, MiEvent, MipiNode, Output, Planes,
    };

    use super::{Hardware, Settings};

    fn io_status(e: &io::Error) -> Status {
        match e.kind() {
            io::ErrorKind::NotFound => Status::NO_DEVICE,
            io::ErrorKind::InvalidInput => Status::INVALID_ARGUMENT,
            _ => Status::IO,
        }
    }

    fn status(e: &CaptureError) -> Status {
        match e {
            CaptureError::Io(e) => io_status(e),
            CaptureError::Dma(_) => Status::IO,
            CaptureError::Memory(_) => Status::INVALID_ARGUMENT,
            // Memory too small for a new layout cannot be given back within the grace after the
            // path was turned off: worth asking again later.
            CaptureError::Refused => Status::BUSY,
        }
    }

    /// The vendor kernel's `vvcam-isp` and `vvcam-mipi` devices, and the host's DMA memory.
    pub struct LinuxHardware {
        settings: Settings,
        /// The DMA memory until the first open, which hands it to the capture.
        dma: Option<Dma>,
        capture: Option<Capture>,
        mipi: Option<MipiNode>,
    }

    impl LinuxHardware {
        /// The devices at `settings`, with `dma` for the frame memory.
        #[must_use]
        pub fn new(settings: Settings, dma: Dma) -> Self {
            Self {
                settings,
                dma: Some(dma),
                capture: None,
                mipi: None,
            }
        }

        fn capture(&mut self) -> Result<&mut Capture> {
            self.capture.as_mut().ok_or(Status::IO)
        }
    }

    impl Hardware for LinuxHardware {
        fn open(&mut self) -> Result<()> {
            if let Some(capture) = self.capture.as_mut() {
                capture.reopen(&self.settings.isp).map_err(|e| status(&e))?;
            } else {
                let dma = self.dma.take().ok_or(Status::IO)?;
                match Capture::open(&self.settings.isp, dma) {
                    Ok(capture) => self.capture = Some(capture),
                    Err((e, dma)) => {
                        self.dma = Some(dma);
                        return Err(io_status(&e));
                    }
                }
            }
            self.capture()?.watch_frames().map_err(|e| status(&e))?;
            self.mipi = Some(MipiNode::open(&self.settings.mipi).map_err(|e| io_status(&e))?);
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
            self.capture()?.write(addr, value).map_err(|e| status(&e))
        }

        fn provide(&mut self, bytes: u64) -> Result<()> {
            self.capture()?.provide(bytes).map_err(|e| status(&e))
        }

        fn start(&mut self, output: Output, planes: Planes) -> Result<()> {
            self.capture()?
                .start(output, planes)
                .map_err(|e| status(&e))
        }

        fn rearm(&mut self, slot: usize) -> Result<()> {
            self.capture()?.rearm(slot).map_err(|e| status(&e))
        }

        fn active(&mut self) -> Result<Option<usize>> {
            self.capture()?.active().map_err(|e| status(&e))
        }

        fn take(&mut self, slot: usize) -> Result<()> {
            self.capture()?.take(slot).map_err(|e| status(&e))
        }

        fn copy_out(&mut self, slot: usize, offset: u64, out: &mut [u8]) -> Result<()> {
            self.capture()?
                .copy_out(slot, offset, out)
                .map_err(|e| status(&e))
        }

        fn stop(&mut self) -> Result<()> {
            match self.capture.as_mut() {
                Some(capture) => capture.stop().map_err(|e| status(&e)),
                None => Ok(()),
            }
        }

        fn next_event(&mut self) -> Result<Option<MiEvent>> {
            self.capture()?.next_event().map_err(|e| status(&e))
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::LinuxHardware;
