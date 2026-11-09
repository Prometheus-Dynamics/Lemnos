//! The block as the driver sees it, and the Linux implementation over the kernel node.

use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::PathBuf;

use lemnos_contract_camera_receiver::Counters;
use lemnos_core::{Result, Status};
use lemnos_driver_rp1_csi2_sys::{self as sys, Node, Start, StreamParams, UNPACK16};
use lemnos_linux_sys::dma_heap;

/// What a stream start names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartParams {
    pub lanes: u32,
    pub mbps: u32,
    pub vc: u32,
    pub data_type: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub size: u32,
    pub unpack16: bool,
    pub buffers: u32,
    pub frame_slots: u32,
}

/// What the driver asks of the block. The real one is [`LinuxHardware`]; tests use a stand-in.
pub trait Hardware: Send {
    /// Data lanes wired.
    fn max_lanes(&mut self) -> Result<u8>;
    /// Makes the stream and starts receiving into it.
    fn start(&mut self, params: &StartParams) -> Result<()>;
    /// Stops receiving and returns every buffer; the kernel waits for the DMA.
    fn stop(&mut self) -> Result<()>;
    /// Whether the stream is still running.
    fn check(&mut self) -> Result<()>;
    /// The module's counters since the last start.
    fn counters(&mut self) -> Result<Counters>;
}

/// Where the block is and what to allocate from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Device name from `/dev/lemnos-rp1-csi2-<device>`, or `None` to use the only one.
    device: Option<String>,
    heap: String,
}

impl Settings {
    /// `INVALID_ARGUMENT` for a device or heap name that could name a path elsewhere.
    pub fn new(device: Option<&str>, heap: &str) -> Result<Self> {
        let ok = |name: &str| !name.is_empty() && !name.contains('/') && !name.contains('\0');
        if !ok(heap) || device.is_some_and(|d| !ok(d)) {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self {
            device: device.map(str::to_owned),
            heap: heap.to_owned(),
        })
    }

    fn node_path(&self) -> io::Result<PathBuf> {
        const PREFIX: &str = "lemnos-rp1-csi2-";

        if let Some(device) = &self.device {
            return Ok(PathBuf::from(format!("/dev/{PREFIX}{device}")));
        }
        let mut found = std::fs::read_dir("/dev")?
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(PREFIX))
            .map(|e| e.path());
        match (found.next(), found.next()) {
            (Some(path), None) => Ok(path),
            (None, _) => Err(io::ErrorKind::NotFound.into()),
            (Some(_), Some(_)) => Err(io::ErrorKind::InvalidInput.into()),
        }
    }

    fn heap_path(&self) -> PathBuf {
        PathBuf::from(format!("/dev/dma_heap/{}", self.heap))
    }
}

/// The block through `/dev/lemnos-rp1-csi2-<device>`. Opened as it's needed, so a block not yet
/// bound is a failed call rather than a failed bind, and a call can succeed once it is.
pub struct LinuxHardware {
    settings: Settings,
    node: Option<Node>,
    /// The running stream and its pool, held until stopped.
    running: Option<(OwnedFd, OwnedFd)>,
}

impl LinuxHardware {
    #[must_use]
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            node: None,
            running: None,
        }
    }

    fn node(&mut self) -> Result<&Node> {
        if self.node.is_none() {
            let node = Node::open(self.settings.node_path().map_err(status)?).map_err(status)?;
            self.node = Some(node);
        }
        self.node.as_ref().ok_or(Status::NO_DEVICE)
    }
}

fn status(error: io::Error) -> Status {
    match error.kind() {
        io::ErrorKind::NotFound => Status::NO_DEVICE,
        io::ErrorKind::PermissionDenied => Status::ACCESS,
        io::ErrorKind::InvalidInput => Status::INVALID_ARGUMENT,
        io::ErrorKind::ResourceBusy => Status::BUSY,
        _ => Status::IO,
    }
}

/// A pool buffer's offset from the next, `LEMNOS_STREAM_BUFFER_ALIGN`.
const POOL_ALIGN: u32 = 4096;

impl Hardware for LinuxHardware {
    fn max_lanes(&mut self) -> Result<u8> {
        let info = self.node()?.info().map_err(status)?;
        u8::try_from(info.max_lanes).map_err(|_| Status::IO)
    }

    fn start(&mut self, params: &StartParams) -> Result<()> {
        let size = params.size;
        let stride = size.next_multiple_of(POOL_ALIGN);
        let pool_bytes = u64::from(stride) * u64::from(params.buffers);
        let heap = std::fs::File::open(self.settings.heap_path()).map_err(status)?;
        let pool = dma_heap::allocate(heap.as_fd(), pool_bytes).map_err(status)?;
        let stream = self
            .node()?
            .stream_create(
                &StreamParams {
                    buffers: params.buffers,
                    buffer_bytes: size,
                    frame_slots: params.frame_slots,
                    job_slots: 8,
                },
                pool.as_fd(),
            )
            .map_err(status)?;
        self.node()?
            .start(&Start {
                stream_fd: stream.as_raw_fd(),
                lanes: params.lanes,
                mbps: params.mbps,
                vc: params.vc,
                dt: params.data_type,
                width: params.width,
                height: params.height,
                stride: params.stride,
                size: params.size,
                flags: if params.unpack16 { UNPACK16 } else { 0 },
                reserved: [0; 2],
            })
            .map_err(status)?;
        self.running = Some((stream, pool));
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        let result = match &self.node {
            Some(node) => node.stop().map_err(status),
            None => Ok(()),
        };
        self.running = None;
        result
    }

    fn check(&mut self) -> Result<()> {
        let info = self.node()?.info().map_err(status)?;
        if info.flags & sys::INFO_WEDGED != 0 || info.streaming == 0 {
            return Err(Status::IO);
        }
        let Some((stream, _)) = &self.running else {
            return Err(Status::IO);
        };
        let state = sys::stream_status(stream.as_fd()).map_err(status)?;
        if state.state != sys::STATE_OPEN {
            return Err(Status::IO);
        }
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters> {
        let info = self.node()?.info().map_err(status)?;
        Ok(Counters {
            frames: info.written,
            dropped: info.dropped,
            errors: info.errors,
        })
    }
}
