//! The engine through `lemnos.ko`'s register jobs (`kernel/lemnos_regjob.h`): a session on the
//! block, the dma-bufs it holds, and jobs run to their end.
//!
//! A job here is register values, the addresses the module is to write and the registers to
//! read back. Nothing in this module checks them, because nothing here needs to be believed:
//! the module copies the job into its own memory and proves every write, every address and
//! every length against the engine's profile and the buffers the session imported before the
//! engine sees any of it. A wrong job from this crate is a refused job.
//!
//! The requests take pointers: [`Session::run`] passes the slices it is given, with their
//! lengths, and the kernel reads exactly that many entries of each and writes exactly as many
//! values as there are reads.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::sync::Arc;

use lemnos_linux_sys::{ior, iow, iowr};

/// `LEMNOS_REGJOB_ABI`.
pub const ABI: u32 = 1;
/// `LEMNOS_REGJOB_PROFILE_A733_VE`: the engine's encoder profile.
pub const PROFILE_A733_VE: u32 = 1;
/// `LEMNOS_REGJOB_MAX_WRITES`.
pub const MAX_WRITES: usize = 1024;
/// `LEMNOS_REGJOB_MAX_ADDRS`.
pub const MAX_ADDRS: usize = 16;
/// `LEMNOS_REGJOB_MAX_READS`.
pub const MAX_READS: usize = 64;
/// `LEMNOS_REGJOB_MAX_BUFFERS`.
pub const MAX_BUFFERS: u32 = 64;
/// `LEMNOS_REGJOB_MAX_TIMEOUT_MS`.
pub const MAX_TIMEOUT_MS: u32 = 10_000;

/// `struct lemnos_next`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Next {
    after: u32,
    block: u32,
    name: [u8; 64],
}

/// `struct lemnos_regjob_open`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Open {
    /// The block's number, in.
    pub block: u32,
    /// 0.
    pub flags: u32,
    /// The session, out.
    pub fd: i32,
    /// `LEMNOS_REGJOB_PROFILE_*`, out.
    pub profile: u32,
    /// The register window jobs may name, out.
    pub window_bytes: u32,
    /// `LEMNOS_REGJOB_ABI`, out.
    pub abi: u32,
}

/// `struct lemnos_regjob_import`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Import {
    /// A dma-buf, in.
    pub fd: i32,
    /// What a job's address names, out.
    pub index: u32,
    /// Its size in the block's address space, out.
    pub bytes: u64,
    /// Its device address, out, for information.
    pub addr: u64,
}

/// `struct lemnos_regjob_write`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Write {
    /// Bytes into the window.
    pub offset: u32,
    /// The value.
    pub value: u32,
}

/// `struct lemnos_regjob_addr`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Addr {
    /// An address register of the profile.
    pub reg: u32,
    /// An imported buffer's index.
    pub buffer: u32,
    /// Bytes into the buffer.
    pub offset: u64,
    /// What the job may reach from there.
    pub bytes: u64,
}

/// `struct lemnos_regjob_run`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Run {
    writes: u64,
    addrs: u64,
    reads: u64,
    values: u64,
    nwrites: u32,
    naddrs: u32,
    nreads: u32,
    start: u32,
    timeout_ms: u32,
    status: u32,
}

/// `struct lemnos_regjob_info`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// `LEMNOS_REGJOB_PROFILE_*`.
    pub profile: u32,
    /// The register window.
    pub window_bytes: u32,
    /// Nonzero: buffers are held until a reboot.
    pub wedged: u32,
    /// Buffers the block may still be using.
    pub live: u32,
    /// Jobs finished.
    pub jobs: u64,
    /// Jobs refused by the checks.
    pub refused: u64,
    /// Jobs that did not finish in time.
    pub timeouts: u64,
    /// Resets after a timeout.
    pub resets: u64,
}

const IOC_NEXT: u32 = iowr::<Next>(b'L', 0xcf);
/// `LEMNOS_IOC_REGJOB_OPEN`.
pub const IOC_OPEN: u32 = iowr::<Open>(b'L', 0xf0);
/// `LEMNOS_REGJOB_IOC_IMPORT`.
pub const IOC_IMPORT: u32 = iowr::<Import>(b'L', 0xf1);
/// `LEMNOS_REGJOB_IOC_RELEASE`.
pub const IOC_RELEASE: u32 = iow::<u32>(b'L', 0xf2);
/// `LEMNOS_REGJOB_IOC_RUN`.
pub const IOC_RUN: u32 = iowr::<Run>(b'L', 0xf3);
/// `LEMNOS_REGJOB_IOC_INFO`.
pub const IOC_INFO: u32 = ior::<Info>(b'L', 0xf4);

/// The module's control node, shared in the process (`lemnos_linux_sys::module::open_shared`).
#[derive(Debug)]
pub struct Control {
    fd: Arc<OwnedFd>,
}

impl Control {
    /// Opens the node at `path`, or shares the process's open file on it.
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            fd: lemnos_linux_sys::module::open_shared(path)?,
        })
    }

    /// The number of the block whose device name is `name`.
    pub fn find_block(&self, name: &str) -> io::Result<u32> {
        let mut next = Next {
            after: 0,
            block: 0,
            name: [0; 64],
        };
        while ioctl(self.fd.as_fd(), IOC_NEXT, (&raw mut next).cast()).is_ok() {
            let end = next.name.iter().position(|&b| b == 0).unwrap_or(64);
            if next.name.get(..end) == Some(name.as_bytes()) {
                return Ok(next.block);
            }
            next.after = next.block;
        }
        Err(io::ErrorKind::NotFound.into())
    }

    /// Opens the block's one session.
    pub fn open_session(&self, block: u32) -> io::Result<Session> {
        let mut open = Open {
            block,
            fd: -1,
            ..Open::default()
        };
        ioctl(self.fd.as_fd(), IOC_OPEN, (&raw mut open).cast())?;
        // SAFETY: a successful open returns a new close-on-exec descriptor nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(open.fd) };
        Ok(Session {
            fd,
            profile: open.profile,
            window_bytes: open.window_bytes,
        })
    }
}

/// One session on a block: its buffers are the only memory its jobs can name. Closing it
/// releases them, after the module has stopped the block if it may still be using one.
#[derive(Debug)]
pub struct Session {
    fd: OwnedFd,
    profile: u32,
    window_bytes: u32,
}

/// One job, as [`Session::run`] passes it.
#[derive(Debug, Clone, Copy)]
pub struct Job<'a> {
    /// The register values, in the order they are written.
    pub writes: &'a [Write],
    /// One address for each address register of the profile.
    pub addrs: &'a [Addr],
    /// Registers read back after the job.
    pub reads: &'a [u32],
    /// What starts it.
    pub start: u32,
    /// How long the block has, in milliseconds.
    pub timeout_ms: u32,
}

impl Session {
    /// `LEMNOS_REGJOB_PROFILE_*`.
    #[must_use]
    pub fn profile(&self) -> u32 {
        self.profile
    }

    /// The register window's size.
    #[must_use]
    pub fn window_bytes(&self) -> u32 {
        self.window_bytes
    }

    /// Imports `buffer`, attached and mapped for the block until it is released.
    pub fn import(&self, buffer: BorrowedFd<'_>) -> io::Result<Import> {
        let mut import = Import {
            fd: buffer.as_raw_fd(),
            ..Import::default()
        };
        ioctl(self.fd.as_fd(), IOC_IMPORT, (&raw mut import).cast())?;
        Ok(import)
    }

    /// Releases buffer `index`. `EBUSY` while the block may still use it.
    pub fn release(&self, index: u32) -> io::Result<()> {
        let mut index = index;
        ioctl(self.fd.as_fd(), IOC_RELEASE, (&raw mut index).cast()).map(|_| ())
    }

    /// Runs `job` to its end. The registers it reads land in `values`, which must be exactly as
    /// long as `job.reads` (`InvalidInput` otherwise, with no request). The status the block's
    /// interrupt gave, or the module's errno: a refusal leaves the block untouched, and
    /// `ETIMEDOUT` comes after the module reset it.
    pub fn run(&self, job: &Job<'_>, values: &mut [u32]) -> io::Result<u32> {
        let count = |n: usize, max: usize| {
            if n <= max {
                u32::try_from(n).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
            } else {
                Err(io::ErrorKind::InvalidInput.into())
            }
        };
        if values.len() != job.reads.len() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut run = Run {
            writes: job.writes.as_ptr() as u64,
            addrs: job.addrs.as_ptr() as u64,
            reads: job.reads.as_ptr() as u64,
            values: values.as_mut_ptr() as u64,
            nwrites: count(job.writes.len(), MAX_WRITES)?,
            naddrs: count(job.addrs.len(), MAX_ADDRS)?,
            nreads: count(job.reads.len(), MAX_READS)?,
            start: job.start,
            timeout_ms: job.timeout_ms,
            status: 0,
        };
        ioctl(self.fd.as_fd(), IOC_RUN, (&raw mut run).cast())?;
        Ok(run.status)
    }

    /// The block's profile and counters.
    pub fn info(&self) -> io::Result<Info> {
        let mut info = Info::default();
        ioctl(self.fd.as_fd(), IOC_INFO, (&raw mut info).cast())?;
        Ok(info)
    }
}

fn ioctl(fd: BorrowedFd<'_>, request: u32, arg: *mut libc::c_void) -> io::Result<libc::c_int> {
    loop {
        // SAFETY: every `request` here is paired, by the callers above, with an `arg` that
        // points at the structure the kernel expects, live and exclusively borrowed for the
        // call. `IOC_RUN`'s structure carries the addresses and lengths of slices borrowed for
        // the call (the values exclusively), and the kernel reads and writes no more entries
        // than those lengths. `fd` is live.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg) };
        if ret >= 0 {
            return Ok(ret);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// The same numbers as `kernel/test/lemnos_regjob_check.c`'s static asserts.
    #[test]
    fn the_layouts_and_the_numbers_are_the_modules() {
        assert_eq!(size_of::<Open>(), 24);
        assert_eq!(size_of::<Import>(), 24);
        assert_eq!(offset_of!(Import, bytes), 8);
        assert_eq!(size_of::<Write>(), 8);
        assert_eq!(size_of::<Addr>(), 24);
        assert_eq!(offset_of!(Addr, offset), 8);
        assert_eq!(size_of::<Run>(), 56);
        assert_eq!(offset_of!(Run, start), 44);
        assert_eq!(offset_of!(Run, status), 52);
        assert_eq!(size_of::<Info>(), 48);
        assert_eq!(IOC_OPEN, 0xc018_4cf0);
        assert_eq!(IOC_IMPORT, 0xc018_4cf1);
        assert_eq!(IOC_RELEASE, 0x4004_4cf2);
        assert_eq!(IOC_RUN, 0xc038_4cf3);
        assert_eq!(IOC_INFO, 0x8030_4cf4);
    }

    #[test]
    fn a_run_whose_values_dont_match_its_reads_is_not_issued() {
        // A plain file takes no request: had one been issued, the error would be ENOTTY.
        let file = std::fs::File::open("/dev/null").unwrap();
        let session = Session {
            fd: OwnedFd::from(file),
            profile: 0,
            window_bytes: 0,
        };
        let job = Job {
            writes: &[],
            addrs: &[],
            reads: &[0, 4],
            start: 0,
            timeout_ms: 1,
        };
        let err = session.run(&job, &mut [0]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let too_many = [Write::default(); MAX_WRITES + 1];
        let job = Job {
            writes: &too_many,
            ..job
        };
        let err = session.run(&job, &mut [0, 0]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let job = Job { writes: &[], ..job };
        let err = session.run(&job, &mut [0, 0]).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ENOTTY));
    }
}
