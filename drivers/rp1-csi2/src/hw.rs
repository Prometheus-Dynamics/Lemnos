//! The block as the driver sees it, and the Linux implementation over the kernel node.

use std::io;
use std::os::fd::BorrowedFd;
use std::path::PathBuf;

use lemnos_contract_camera_receiver::{Counters, FrameStarts};
use lemnos_core::{Result, Status};
use lemnos_driver_rp1_csi2_sys::{self as sys, Node, Start, Stream, StreamParams, UNPACK16};

use crate::relay::Set;

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

/// A frame the block finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finished {
    /// The buffer it is in.
    pub buffer: u32,
    /// Bytes.
    pub bytes: u32,
    /// Counts frame starts from the start of the stream.
    pub sequence: u64,
    /// When the frame started.
    pub timestamp_ns: u64,
    /// What giving the buffer back must carry.
    pub fence: u32,
}

/// What the driver asks of the block. The real one is [`LinuxHardware`]; tests use a stand-in.
pub trait Hardware: Send {
    /// Data lanes wired.
    fn max_lanes(&mut self) -> Result<u8>;
    /// Ends whatever ran and makes the module's stream, with no buffer having memory yet.
    /// Nothing is received.
    fn make(&mut self, params: &StartParams) -> Result<()>;
    /// Starts receiving into the stream [`make`](Self::make) made.
    fn go(&mut self) -> Result<()>;
    /// Stops receiving and ends the module's stream; the kernel waits for the DMA, and then
    /// lets go of every buffer's memory (or of none, if the DMA couldn't be seen to stop).
    fn stop(&mut self) -> Result<()>;
    /// Gives `buffer` its memory, or swaps it: the module pins `memory` and, from then on,
    /// that is where the block writes the buffer's frames. An error means the stream is over.
    fn set_buffer(&mut self, buffer: u32, memory: BorrowedFd<'_>) -> Result<Set>;
    /// The next frame the block finished.
    fn next_frame(&mut self) -> Result<Option<Finished>>;
    /// Gives a finished frame's buffer back to the block.
    fn give_back(&mut self, buffer: u32, fence: u32) -> Result<()>;
    /// Whether the stream is still running.
    fn check(&mut self) -> Result<()>;
    /// The module's counters since the last start.
    fn counters(&mut self) -> Result<Counters>;
    /// The frame-start count since the last start, or `None` while nothing is streaming (not
    /// started, or stopped): unknown, and not a count of zero.
    fn frame_starts(&mut self) -> Result<Option<FrameStarts>>;
}

/// Where the block is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Device name from `/dev/lemnos-rp1-csi2-<device>`, or `None` to use the only one.
    device: Option<String>,
}

impl Settings {
    /// `INVALID_ARGUMENT` for a device name that could name a path elsewhere.
    pub fn new(device: Option<&str>) -> Result<Self> {
        let ok = |name: &str| !name.is_empty() && !name.contains('/') && !name.contains('\0');
        if device.is_some_and(|d| !ok(d)) {
            return Err(Status::INVALID_ARGUMENT);
        }
        Ok(Self {
            device: device.map(str::to_owned),
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
}

/// The block through `/dev/lemnos-rp1-csi2-<device>`. Opened as it's needed, so a block not yet
/// bound is a failed call rather than a failed bind, and a call can succeed once it is.
pub struct LinuxHardware {
    settings: Settings,
    node: Option<Node>,
    /// The module's stream and what it is started with, held until stopped. Dropping the
    /// stream ends it: the module stops the block first and then unpins every buffer.
    running: Option<(Stream, StartParams)>,
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

    /// An error from the node. A node whose block was unbound answers `ENODEV` to everything
    /// for good, and a block bound again is a new node at the same path: the old one is
    /// dropped, so the next call opens the new one.
    fn node_error(&mut self, error: io::Error) -> Status {
        if node_is_gone(&error) {
            self.node = None;
            self.running = None;
        }
        status(error)
    }
}

/// Whether the node answered that its block is gone.
fn node_is_gone(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc_errno::ENODEV)
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

impl Hardware for LinuxHardware {
    fn max_lanes(&mut self) -> Result<u8> {
        let info = self.node()?.info().map_err(status)?;
        u8::try_from(info.max_lanes).map_err(|_| Status::IO)
    }

    fn make(&mut self, params: &StartParams) -> Result<()> {
        let _ = self.stop();
        let made = self.node()?.stream_create(&StreamParams {
            buffers: params.buffers,
            buffer_bytes: params.size,
            frame_slots: params.frame_slots,
            job_slots: 8,
        });
        let stream = made.map_err(|e| self.node_error(e))?;
        self.running = Some((stream, *params));
        Ok(())
    }

    fn go(&mut self) -> Result<()> {
        let Some((stream, params)) = &self.running else {
            return Err(Status::NO_DEVICE);
        };
        let (stream_fd, params) = (stream.raw_fd(), *params);
        let started = self.node()?.start(&Start {
            stream_fd,
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
        });
        started.map_err(|e| self.node_error(e))
    }

    fn stop(&mut self) -> Result<()> {
        let result = match &self.node {
            Some(node) => node.stop(),
            None => Ok(()),
        };
        self.running = None;
        match result {
            Ok(()) => Ok(()),
            // Unbinding stopped the block: there's nothing left to stop.
            Err(e) if node_is_gone(&e) => {
                self.node = None;
                Ok(())
            }
            Err(e) => Err(status(e)),
        }
    }

    fn check(&mut self) -> Result<()> {
        let info = self.node()?.info().map_err(status)?;
        if info.flags & sys::INFO_WEDGED != 0 || info.streaming == 0 {
            return Err(Status::IO);
        }
        let Some((stream, _)) = &self.running else {
            return Err(Status::IO);
        };
        let state = stream.status().map_err(status)?;
        if state.state != sys::STATE_OPEN {
            return Err(Status::IO);
        }
        Ok(())
    }

    fn set_buffer(&mut self, buffer: u32, memory: BorrowedFd<'_>) -> Result<Set> {
        let Some((stream, _)) = &self.running else {
            return Err(Status::NO_DEVICE);
        };
        match stream.set_buffer(buffer, memory) {
            Ok(_) => Ok(Set::Done),
            Err(e) => match e.raw_os_error() {
                // The block has the buffer, or the dma-buf is another buffer's.
                Some(libc_errno::EBUSY) => Ok(Set::Busy),
                Some(libc_errno::EINVAL | libc_errno::EBADF) => Ok(Set::Refused),
                _ => Err(status(e)),
            },
        }
    }

    fn next_frame(&mut self) -> Result<Option<Finished>> {
        let Some((stream, _)) = &mut self.running else {
            return Err(Status::NO_DEVICE);
        };
        Ok(stream.next_frame().map_err(status)?.map(|f| Finished {
            buffer: f.buffer,
            bytes: f.bytes,
            sequence: f.sequence,
            timestamp_ns: f.timestamp_ns,
            fence: f.fence,
        }))
    }

    fn give_back(&mut self, buffer: u32, fence: u32) -> Result<()> {
        let Some((stream, _)) = &mut self.running else {
            return Err(Status::NO_DEVICE);
        };
        stream.return_buffer(buffer, fence).map_err(status)
    }

    fn counters(&mut self) -> Result<Counters> {
        let info = self.node()?.info().map_err(status)?;
        Ok(Counters {
            frames: info.written,
            dropped: info.dropped,
            errors: info.errors,
        })
    }

    fn frame_starts(&mut self) -> Result<Option<FrameStarts>> {
        let read = self.node()?.frame_starts();
        let fs = match read {
            Ok(fs) => fs,
            // A module without the ioctl can't count its frame starts: that is the receiver's
            // fact, not a failed read.
            Err(e) if e.raw_os_error() == Some(libc_errno::ENOTTY) => {
                return Err(Status::NOT_SUPPORTED);
            }
            Err(e) => return Err(self.node_error(e)),
        };
        Ok((fs.valid != 0).then_some(FrameStarts {
            count: fs.frames,
            latest_ns: fs.latest_ns,
        }))
    }
}

/// The errnos the module answers a buffer's memory with (`kernel/lemnos.h`).
mod libc_errno {
    pub const EBADF: i32 = 9;
    pub const EBUSY: i32 = 16;
    pub const ENODEV: i32 = 19;
    pub const EINVAL: i32 = 22;
    pub const ENOTTY: i32 = 25;
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::{libc_errno, node_is_gone};

    #[test]
    fn only_enodev_means_the_node_is_gone() {
        assert!(node_is_gone(&io::Error::from_raw_os_error(
            libc_errno::ENODEV
        )));
        for errno in [libc_errno::EBUSY, libc_errno::EINVAL, libc_errno::EBADF, 5] {
            assert!(!node_is_gone(&io::Error::from_raw_os_error(errno)));
        }
        assert!(!node_is_gone(&io::ErrorKind::NotFound.into()));
    }
}
