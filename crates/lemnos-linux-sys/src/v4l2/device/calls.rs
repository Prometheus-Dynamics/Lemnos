//! The V4L2 calls whose arguments hold pointers the kernel follows or descriptors
//! it creates, so a plain [`Ioctl`](crate::ioctl::Ioctl) cannot stand for them:
//! buffers with their plane arrays, extended controls with their payloads, and
//! exported dma-bufs. Each function sets the pointers itself from borrowed slices.

use std::io;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use super::uapi::{
    MEMORY_USERPTR, VIDEO_MAX_PLANES, VIDIOC_EXPBUF, v4l2_buffer, v4l2_exportbuffer,
    v4l2_ext_control, v4l2_ext_control_value, v4l2_ext_controls, v4l2_m, v4l2_plane,
};
use crate::ioctl::{ioctl_ptr, iowr};

fn einval() -> io::Error {
    io::Error::from_raw_os_error(libc::EINVAL)
}

/// A request that takes a `struct v4l2_buffer`.
#[derive(Clone, Copy, Debug)]
pub struct BufferIoctl {
    nr: u32,
    name: &'static str,
}

/// `VIDIOC_QUERYBUF`.
pub const VIDIOC_QUERYBUF: BufferIoctl = BufferIoctl {
    nr: iowr::<v4l2_buffer>(b'V', 9),
    name: "VIDIOC_QUERYBUF",
};
/// `VIDIOC_QBUF`.
pub const VIDIOC_QBUF: BufferIoctl = BufferIoctl {
    nr: iowr::<v4l2_buffer>(b'V', 15),
    name: "VIDIOC_QBUF",
};
/// `VIDIOC_DQBUF`.
pub const VIDIOC_DQBUF: BufferIoctl = BufferIoctl {
    nr: iowr::<v4l2_buffer>(b'V', 17),
    name: "VIDIOC_DQBUF",
};
/// `VIDIOC_PREPARE_BUF`.
pub const VIDIOC_PREPARE_BUF: BufferIoctl = BufferIoctl {
    nr: iowr::<v4l2_buffer>(b'V', 93),
    name: "VIDIOC_PREPARE_BUF",
};

impl BufferIoctl {
    /// The request number.
    pub const fn nr(self) -> u32 {
        self.nr
    }

    /// The request's uAPI name, for error messages.
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Issues the request on `buf`. On a multi-planar queue `planes` are the
    /// buffer's planes, read and written by the kernel, and `buf.length` says how
    /// many on return; on a single-planar queue `planes` must be empty. User-pointer
    /// memory is refused (`EINVAL`): nothing here hands the kernel an address to
    /// write to.
    pub fn call(
        self,
        fd: BorrowedFd<'_>,
        buf: &mut v4l2_buffer,
        planes: &mut [v4l2_plane],
    ) -> io::Result<()> {
        let multi = buf.is_multiplanar();
        if buf.memory == MEMORY_USERPTR
            || planes.len() > VIDEO_MAX_PLANES
            || (!multi && !planes.is_empty())
        {
            return Err(einval());
        }
        if multi {
            buf.m = v4l2_m {
                planes: planes.as_mut_ptr(),
            };
            buf.length = planes.len() as u32;
        }
        // SAFETY: the request takes a `v4l2_buffer`, which `buf` is. On a
        // multi-planar queue `m` points at `length` planes in `planes`, alive for
        // the call; their `m` members can only be offsets and descriptors. On a
        // single-planar queue `m` is an offset or a descriptor. User pointers were
        // refused above.
        let result = unsafe { ioctl_ptr(fd, self.nr, std::ptr::from_mut(buf)) };
        if multi {
            // `planes` is about to go back to the caller: keep no pointer to it.
            buf.m = v4l2_m { offset: 0 };
        }
        result.map(|_| ())
    }
}

/// Exports one plane of an MMAP buffer as a dma-buf (`VIDIOC_EXPBUF`); `raw` says
/// which plane and with which open flags.
pub fn export_buffer(fd: BorrowedFd<'_>, raw: &mut v4l2_exportbuffer) -> io::Result<OwnedFd> {
    VIDIOC_EXPBUF.call(fd, raw)?;
    if raw.fd < 0 {
        return Err(io::Error::other("VIDIOC_EXPBUF returned no descriptor"));
    }
    // SAFETY: a successful export returns a new dma-buf descriptor, which no other
    // owner has.
    Ok(unsafe { OwnedFd::from_raw_fd(raw.fd) })
}

/// A request that takes a `struct v4l2_ext_controls`.
#[derive(Clone, Copy, Debug)]
pub struct ExtCtrlsIoctl {
    nr: u32,
    name: &'static str,
}

/// `VIDIOC_G_EXT_CTRLS`.
pub const VIDIOC_G_EXT_CTRLS: ExtCtrlsIoctl = ExtCtrlsIoctl {
    nr: iowr::<v4l2_ext_controls>(b'V', 71),
    name: "VIDIOC_G_EXT_CTRLS",
};
/// `VIDIOC_S_EXT_CTRLS`.
pub const VIDIOC_S_EXT_CTRLS: ExtCtrlsIoctl = ExtCtrlsIoctl {
    nr: iowr::<v4l2_ext_controls>(b'V', 72),
    name: "VIDIOC_S_EXT_CTRLS",
};
/// `VIDIOC_TRY_EXT_CTRLS`.
pub const VIDIOC_TRY_EXT_CTRLS: ExtCtrlsIoctl = ExtCtrlsIoctl {
    nr: iowr::<v4l2_ext_controls>(b'V', 73),
    name: "VIDIOC_TRY_EXT_CTRLS",
};

/// One control of an extended-controls call: `struct v4l2_ext_control`, with the
/// payload it points at borrowed for `'a`. The payload is exclusive because the
/// kernel may write it on any of the calls, a set included.
#[repr(transparent)]
#[derive(Debug)]
pub struct ExtControl<'a> {
    raw: v4l2_ext_control,
    payload: PhantomData<&'a mut [u8]>,
}

impl std::fmt::Debug for v4l2_ext_control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (id, size) = (self.id, self.size);
        write!(f, "v4l2_ext_control({id:#x}, {size} bytes)")
    }
}

impl ExtControl<'static> {
    fn of(id: u32, u: v4l2_ext_control_value) -> Self {
        Self {
            raw: v4l2_ext_control {
                id,
                size: 0,
                reserved2: [0],
                u,
            },
            payload: PhantomData,
        }
    }

    /// A 32-bit control (an integer, boolean, menu index, bitmask or button).
    pub fn value(id: u32, value: i32) -> Self {
        // Through the 64-bit member, so the union's other bytes are zero.
        Self::of(
            id,
            v4l2_ext_control_value {
                value64: i64::from(value as u32),
            },
        )
    }

    /// A 64-bit control.
    pub fn value64(id: u32, value64: i64) -> Self {
        Self::of(id, v4l2_ext_control_value { value64 })
    }
}

impl<'a> ExtControl<'a> {
    /// A string, array or compound control over `payload`: the kernel reads it on
    /// a set, and fills it on a get.
    pub fn payload(id: u32, payload: &'a mut [u8]) -> Self {
        Self {
            raw: v4l2_ext_control {
                id,
                size: payload.len() as u32,
                reserved2: [0],
                u: v4l2_ext_control_value {
                    ptr: payload.as_mut_ptr().cast(),
                },
            },
            payload: PhantomData,
        }
    }

    /// The control's id.
    pub fn id(&self) -> u32 {
        self.raw.id
    }

    /// The payload's size in bytes. A dynamic array reports the used size here.
    pub fn size(&self) -> u32 {
        self.raw.size
    }

    /// The 32-bit value after a get.
    pub fn get_value(&self) -> i32 {
        // SAFETY: the union's first four bytes are a plain integer.
        unsafe { self.raw.u.value }
    }

    /// The 64-bit value after a get.
    pub fn get_value64(&self) -> i64 {
        // SAFETY: the union's eight bytes are a plain integer.
        unsafe { self.raw.u.value64 }
    }
}

/// A refused extended-controls call: the error, and the kernel's `error_idx`.
#[derive(Debug)]
pub struct ExtCtrlsError {
    /// What the call failed with.
    pub source: io::Error,
    /// The index of the control the kernel blames; `controls.len()` when it blames
    /// none of them.
    pub error_idx: u32,
}

impl ExtCtrlsIoctl {
    /// The request's uAPI name, for error messages.
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// The request number.
    pub const fn nr(self) -> u32 {
        self.nr
    }

    /// Issues the request on `controls`, with `which` and (for request values) the
    /// media request in `request`.
    pub fn call(
        self,
        fd: BorrowedFd<'_>,
        which: u32,
        request: Option<BorrowedFd<'_>>,
        controls: &mut [ExtControl<'_>],
    ) -> Result<(), ExtCtrlsError> {
        let mut raw = v4l2_ext_controls {
            which,
            count: controls.len() as u32,
            error_idx: 0,
            request_fd: request.map_or(0, |r| r.as_raw_fd()),
            reserved: [0],
            controls: controls.as_mut_ptr().cast::<v4l2_ext_control>(),
        };
        // SAFETY: the request takes a `v4l2_ext_controls`, which `raw` is. `controls`
        // points at `count` entries (`ExtControl` is a transparent
        // `v4l2_ext_control`), alive for the call; each payload pointer comes from a
        // `&mut [u8]` of `size` bytes that `'_` keeps borrowed.
        match unsafe { ioctl_ptr(fd, self.nr, &raw mut raw) } {
            Ok(_) => Ok(()),
            Err(source) => Err(ExtCtrlsError {
                source,
                error_idx: raw.error_idx,
            }),
        }
    }
}

/// Sets or clears `O_NONBLOCK` on a descriptor. Left alone when already so.
pub fn set_nonblocking(fd: BorrowedFd<'_>, nonblocking: bool) -> io::Result<()> {
    // SAFETY: `fcntl` with `F_GETFL` takes no pointer.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let wanted = if nonblocking {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    if wanted == flags {
        return Ok(());
    }
    // SAFETY: `fcntl` with `F_SETFL` takes an integer flag set.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, wanted) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
