//! A session on the node: one open file, the objects it made, and the calls.
//!
//! # Memory
//!
//! Every argument is a structure on the stack that the kernel copies in and out during the call
//! (`drm_ioctl` copies the argument both ways for these `DRM_IOWR` requests) and that holds no
//! pointer the kernel follows into user memory. The CPU's view of an object is a shared mapping
//! of the object on this file ([`Mapping`]), unmapped before the object is destroyed and before
//! the file is closed (the table is declared before the file).
//!
//! # Errors
//!
//! `EINTR` is retried. A request the session refuses is `InvalidInput`, before any call. Anything
//! else is the kernel's error.
//!
//! # Allocation
//!
//! Opening allocates the table. Making an object may grow it; runs, queries, destroys and
//! [`NpuSession::memory`] allocate nothing.

use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use lemnos_driver_rknpu::uapi::{self, Action, MemCreate, MemDestroy, MemMap, Submit, TASK_BYTES};
use lemnos_driver_rknpu::{Create, Domain, Kind, Object, Query, Run, Session};
use lemnos_linux_sys::mmap::Mapping;

/// Objects a session holds at once: far more than the driver's programs and slots make.
pub const MAX_OBJECTS: usize = 2048;

/// A uAPI structure and the request that takes it.
pub(crate) trait Request: Copy {
    /// The request number, which encodes this type's size.
    const NR: u32;
}

impl Request for Action {
    const NR: u32 = uapi::IOCTL_ACTION;
}

impl Request for MemCreate {
    const NR: u32 = uapi::IOCTL_MEM_CREATE;
}

impl Request for MemMap {
    const NR: u32 = uapi::IOCTL_MEM_MAP;
}

impl Request for MemDestroy {
    const NR: u32 = uapi::IOCTL_MEM_DESTROY;
}

impl Request for Submit {
    const NR: u32 = uapi::IOCTL_SUBMIT;
}

/// Issues `T::NR` on `fd` with `arg`, retrying on `EINTR`.
fn call<T: Request>(fd: BorrowedFd<'_>, arg: &mut T) -> io::Result<()> {
    loop {
        // SAFETY: `T::NR` is the request whose argument type is `T` (the pairing and the sizes
        // are pinned by the tests); `arg` is a live, exclusively borrowed `T` the kernel copies
        // in and out during this call only, and `T` holds no user pointer the kernel follows.
        // `fd` is open for the duration of the call.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), T::NR as _, std::ptr::from_mut(arg)) };
        if ret >= 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

fn refused(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why)
}

/// One object the session made.
#[derive(Debug)]
struct Held {
    handle: u32,
    /// The kernel's own address of the object, which `SUBMIT` takes back.
    obj_addr: u64,
    iova: u64,
    bytes: u32,
    domain: Domain,
    kind: Kind,
    /// Gone once a destroy has been tried: the object is the kernel's to free from then on.
    map: Option<Mapping>,
}

/// An open NPU node.
#[derive(Debug)]
pub struct NpuSession {
    /// Declared before the file: the mappings go before the file closes.
    held: Vec<Held>,
    fd: OwnedFd,
}

impl NpuSession {
    /// Opens `path` read-write, close-on-exec. Nothing is sent.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)?;
        Ok(Self::over(file.into()))
    }

    /// A session over an open file.
    pub(crate) fn over(fd: OwnedFd) -> Self {
        Self {
            held: Vec::with_capacity(64),
            fd,
        }
    }

    fn find(&self, handle: u32) -> Option<usize> {
        self.held.iter().position(|h| h.handle == handle)
    }

    /// Closes a handle the kernel just gave, after a step past it failed.
    fn abandon(&mut self, handle: u32) {
        let mut arg = MemDestroy {
            handle,
            ..MemDestroy::default()
        };
        let _ = call(self.fd.as_fd(), &mut arg);
    }

    /// Objects held, those whose destroy failed included.
    #[must_use]
    pub fn objects(&self) -> usize {
        self.held.len()
    }
}

impl Session for NpuSession {
    fn query(&mut self, query: Query) -> io::Result<u32> {
        let mut arg = query.to_raw();
        call(self.fd.as_fd(), &mut arg)?;
        Ok(arg.value)
    }

    fn create(&mut self, request: Create) -> io::Result<Object> {
        if self.held.len() >= MAX_OBJECTS {
            return Err(io::Error::from(io::ErrorKind::OutOfMemory));
        }
        let mut arg = request.to_raw();
        call(self.fd.as_fd(), &mut arg)?;
        let handle = arg.handle;
        if handle == 0 || self.find(handle).is_some() {
            return Err(io::Error::other("MEM_CREATE returned no new handle"));
        }
        if arg.size < u64::from(request.bytes()) || arg.obj_addr == 0 {
            self.abandon(handle);
            return Err(io::Error::other("MEM_CREATE made less than asked"));
        }
        let mapped = usize::try_from(arg.size)
            .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))
            .and_then(|size| {
                let mut map = MemMap {
                    handle,
                    ..MemMap::default()
                };
                call(self.fd.as_fd(), &mut map)?;
                Mapping::new(self.fd.as_fd(), size, map.offset, true)
            });
        let map = match mapped {
            Ok(map) => map,
            Err(error) => {
                self.abandon(handle);
                return Err(error);
            }
        };
        self.held.push(Held {
            handle,
            obj_addr: arg.obj_addr,
            iova: arg.dma_addr,
            bytes: request.bytes(),
            domain: request.domain(),
            kind: request.kind(),
            map: Some(map),
        });
        Ok(Object {
            handle,
            iova: arg.dma_addr,
            bytes: request.bytes(),
            size: arg.size,
            domain: request.domain(),
            kind: request.kind(),
        })
    }

    fn memory(&mut self, handle: u32) -> Option<&mut [u8]> {
        let at = self.find(handle)?;
        self.held[at].map.as_mut().map(Mapping::as_mut_slice)
    }

    fn destroy(&mut self, handle: u32) -> io::Result<()> {
        let at = self
            .find(handle)
            .ok_or_else(|| refused("not an object of this session"))?;
        // The mapping holds the object: it goes first, or the object would stay mapped in its
        // domain after the handle is closed.
        self.held[at].map = None;
        let mut arg = MemDestroy {
            handle,
            ..MemDestroy::default()
        };
        call(self.fd.as_fd(), &mut arg)?;
        self.held.swap_remove(at);
        Ok(())
    }

    fn submit(&mut self, run: &Run) -> io::Result<()> {
        let at = self
            .find(run.body())
            .ok_or_else(|| refused("not an object of this session"))?;
        let body = &self.held[at];
        if body.kind != Kind::Body || body.map.is_none() {
            return Err(refused("not a live body"));
        }
        if body.domain != run.domain() {
            return Err(refused("a body run outside its domain"));
        }
        if u64::from(run.tasks()) * u64::from(TASK_BYTES) > u64::from(body.bytes) {
            return Err(refused("tasks past the body"));
        }
        let base = run.task_base();
        if base < body.iova || base - body.iova >= u64::from(body.bytes) {
            return Err(refused("task base outside the body"));
        }
        let mut arg = run.to_raw(body.obj_addr);
        call(self.fd.as_fd(), &mut arg)
    }
}
