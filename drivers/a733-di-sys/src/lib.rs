//! The A733's deinterlacer on the vendor kernel (`/dev/deinterlace`; the path is always the
//! caller's), as far as it can be used soundly from user space. This crate holds the `unsafe`;
//! `lemnos-driver-a733-di` holds the block's state and what a caller is given.
//!
//! - [`uapi`]: the requests and structures, transcribed with their source and pinned by
//!   layout tests.
//! - [`Frame`], [`Config`] and [`Job`]: a request, built only from frames checked against the
//!   real size of the dma-buf each sits in. The kernel driver checks none of it, takes a
//!   negative descriptor as a physical address and leaves a zero chroma offset at address 0,
//!   so nothing else builds one: a [`Job`] can be made only by [`Job::new`], and
//!   [`Node::process`] takes nothing but a [`Job`].
//! - [`Node`]: one open of the device, issuing the requests.
//!
//! # What is issued, and why it is sound
//!
//! Every request takes no argument or one plain structure with no pointer in it. The node
//! passes a pointer to a copy on its own stack, alive for the call; the kernel copies the
//! structure in (and, for the version, out). `DI_IOC_PROCESS_FB` is given only a [`Job`]'s
//! request, in which every descriptor is a dma-buf the caller holds for the job's lifetime
//! and every offset and extent was checked against that dma-buf's real size.
//!
//! # What is never done
//!
//! - The node is never mapped: its `mmap` maps whatever physical page the offset names.
//! - `DI_IOC_MEM_REQUEST` and `DI_IOC_MEM_RELEASE` are never issued: they answer and take
//!   physical addresses.
//! - The tuning tables, the demo crop, the timeouts and `DI_IOC_SET_MEDIAID` are not issued.
//! - A kernel built with `CONFIG_SUNXI_DI_SINGEL_FILE` has larger structures than these
//!   ([`uapi`] says how), and its request numbers differ with them, so it answers every
//!   structure-carrying request here with `ENOTTY`: [`Node::open`] asks the version first and
//!   refuses a device that doesn't answer it.
//!
//! Hardware status: untested on board. The checks are `testing/board-runs/E20.md` and
//! `testing/board-runs/E38.md`.

#![cfg(target_os = "linux")]

mod job;
pub mod uapi;

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub use job::{
    Config, Field, Format, Frame, FrameError, Invalid, Job, Layout, Level, MAX_SIZE, MAX_STRIDE,
    Mode, Settings, Tnr,
};

/// One open of the deinterlacer's node.
#[derive(Debug)]
pub struct Node {
    file: File,
}

impl Node {
    /// Opens the node at `path` read-write and close-on-exec, and asks its version: a device
    /// that doesn't answer is not this driver's, or is built with other structures.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)?;
        let node = Self::on(file);
        node.version()?;
        Ok(node)
    }

    /// The node on an open file. Nothing is asked of it.
    #[must_use]
    pub fn on(file: File) -> Self {
        Self { file }
    }

    /// A request with no argument.
    fn bare(&self, request: u32) -> io::Result<i32> {
        // SAFETY: the request takes no argument (its encoded size is 0, and the kernel's
        // handler reads none); the descriptor is this node's open file.
        let ret = unsafe { libc::ioctl(self.file.as_raw_fd(), libc::c_ulong::from(request), 0) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ret)
    }

    /// A request that carries `argument`, one of the plain structures of [`uapi`], whose size
    /// the request number encodes.
    fn with<T: Copy>(&self, request: u32, argument: &mut T) -> io::Result<i32> {
        // The number was made from `T`, so the kernel copies exactly `T`.
        debug_assert_eq!(((request >> 16) & 0x3fff) as usize, size_of::<T>());
        let pointer: *mut T = argument;
        // SAFETY: the request copies one `T` in from the pointer and, for a read, back out;
        // `pointer` is to a live, exclusively borrowed `T` that outlives the call, and every
        // `T` passed here is a `repr(C)` structure of integers with no pointer in it.
        let ret =
            unsafe { libc::ioctl(self.file.as_raw_fd(), libc::c_ulong::from(request), pointer) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ret)
    }

    /// `DI_IOC_GET_VERSION`.
    pub fn version(&self) -> io::Result<uapi::Version> {
        let mut version = uapi::Version::default();
        self.with(uapi::GET_VERSION, &mut version)?;
        Ok(version)
    }

    /// `DI_IOC_RESET`.
    pub fn reset(&self) -> io::Result<()> {
        self.bare(uapi::RESET).map(|_| ())
    }

    /// `DI_IOC_SET_VIDEO_SIZE`, `DI_IOC_SET_VIDEO_CROP`, `DI_IOC_SET_DIT_MODE`,
    /// `DI_IOC_SET_TNR_MODE` and `DI_IOC_SET_FMD_ENABLE`, in that order, from a checked
    /// configuration's settings.
    pub fn configure(&self, settings: &Settings) -> io::Result<()> {
        let mut settings = *settings;
        self.with(uapi::SET_VIDEO_SIZE, &mut settings.size)?;
        self.with(uapi::SET_VIDEO_CROP, &mut settings.crop)?;
        self.with(uapi::SET_DIT_MODE, &mut settings.dit)?;
        self.with(uapi::SET_TNR_MODE, &mut settings.tnr)?;
        self.with(uapi::SET_FMD_ENABLE, &mut settings.fmd)?;
        Ok(())
    }

    /// `DI_IOC_CHECK_PARA`.
    pub fn check(&self) -> io::Result<()> {
        self.bare(uapi::CHECK_PARA).map(|_| ())
    }

    /// `DI_IOC_PROCESS_FB` with a copy of `job`'s request, answering the call's result. The
    /// job's frames are borrowed for its lifetime, so every descriptor in the request is open
    /// for the call.
    pub fn process(&self, job: &Job<'_>) -> io::Result<i32> {
        let mut request = job.request();
        self.with(uapi::PROCESS_FB, &mut request)
    }
}

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn file(name: &str) -> File {
        let path =
            std::env::temp_dir().join(format!("lemnos-a733-di-sys-{}-{name}", std::process::id()));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(b"not a deinterlacer").unwrap();
        std::fs::remove_file(path).unwrap();
        file
    }

    #[test]
    fn every_request_number_carries_the_size_of_the_structure_it_is_issued_with() {
        let size = |request: u32| ((request >> 16) & 0x3fff) as usize;
        assert_eq!(size(uapi::GET_VERSION), size_of::<uapi::Version>());
        assert_eq!(size(uapi::SET_VIDEO_SIZE), size_of::<uapi::Size>());
        assert_eq!(size(uapi::SET_VIDEO_CROP), size_of::<uapi::Rect>());
        assert_eq!(size(uapi::SET_DIT_MODE), size_of::<uapi::DitMode>());
        assert_eq!(size(uapi::SET_TNR_MODE), size_of::<uapi::TnrMode>());
        assert_eq!(size(uapi::SET_FMD_ENABLE), size_of::<uapi::FmdEnable>());
        assert_eq!(size(uapi::PROCESS_FB), size_of::<uapi::ProcessFbArg>());
        assert_eq!((size(uapi::RESET), size(uapi::CHECK_PARA)), (0, 0));
    }

    #[test]
    fn a_file_that_is_no_deinterlacer_answers_no_request() {
        // A plain file takes none of the requests: the kernel's answer shows each was made.
        // No device node is opened.
        let node = Node::on(file("plain"));
        assert_eq!(
            node.version().unwrap_err().raw_os_error(),
            Some(libc::ENOTTY)
        );
        assert_eq!(node.reset().unwrap_err().raw_os_error(), Some(libc::ENOTTY));
        assert_eq!(node.check().unwrap_err().raw_os_error(), Some(libc::ENOTTY));
        let error = Node::open(Path::new("/nonexistent/deinterlace")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
