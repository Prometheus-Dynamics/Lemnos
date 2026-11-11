//! The engine's node on Linux: a [`Device`] over `lemnos-driver-a733-video-sys`, which holds
//! the `unsafe` and checks every request's argument.

use std::io;
use std::os::fd::BorrowedFd;
use std::path::Path;

use lemnos_driver_a733_video_sys::{Node, Plain};

use crate::block::{Completion, Device, Lock, Mode};

/// One open of the engine's node.
#[derive(Debug)]
pub struct LinuxDevice(Node);

impl LinuxDevice {
    /// Opens the node at `path` and maps its register window, read-only.
    pub fn open(path: &Path) -> io::Result<Self> {
        Node::open(path).map(Self)
    }

    fn plain(&self, request: Plain, value: u32) -> io::Result<()> {
        self.0.plain(request, value).map(|_| ())
    }
}

impl Device for LinuxDevice {
    fn request_engine(&self) -> io::Result<()> {
        self.plain(Plain::EngineRequest, 0)
    }

    fn release_engine(&self) -> io::Result<()> {
        self.plain(Plain::EngineRelease, 0)
    }

    fn lock(&self, lock: Lock) -> io::Result<()> {
        self.plain(Plain::GetLock, lock.bit())
    }

    fn unlock(&self, lock: Lock) -> io::Result<()> {
        self.plain(Plain::ReleaseLock, lock.bit())
    }

    fn reset(&self) -> io::Result<()> {
        self.plain(Plain::Reset, 0)
    }

    fn set_mode(&self, mode: Mode) -> io::Result<()> {
        self.plain(Plain::Mode, mode.value())
    }

    fn map(&self, buffer: BorrowedFd<'_>) -> io::Result<u32> {
        self.0.map_dma_buf(buffer)
    }

    fn unmap(&self, buffer: BorrowedFd<'_>) -> io::Result<()> {
        self.0.unmap_dma_buf(buffer)
    }

    fn clear_completion(&self) -> io::Result<()> {
        self.plain(Plain::ClearEncoderInterrupt, 0)
    }

    fn wait(&self, completion: Completion, seconds: u32) -> io::Result<bool> {
        let request = match completion {
            Completion::Decoder => Plain::WaitDecoder,
            Completion::Encoder => Plain::WaitEncoder,
            Completion::VcuEncoder => Plain::WaitVcuEncoder,
            Completion::VcuDecoder => Plain::WaitVcuDecoder,
        };
        Ok(self.0.plain(request, seconds)? == 1)
    }

    fn read(&self, offset: usize) -> u32 {
        // The block asks only for registers inside the window; one outside reads as 0.
        self.0.read(offset).unwrap_or(0)
    }
}
